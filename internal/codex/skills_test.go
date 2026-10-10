package codex

import (
	"encoding/json"
	"os"
	"strings"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

func TestExtractSkillEventsFunctionCallAndLocatorShape(t *testing.T) {
	model := "gpt-test"
	record := skillTestRecord(`{"type":"response_item","timestamp":"2026-06-01T12:00:00Z","payload":{"type":"function_call","name":"exec_command","arguments":{"cmd":"cat C:\\Users\\me\\.agents\\skills\\alpha\\SKILL.md /workspace/skills/beta/SKILL.md /workspace/skills/nested/child/SKILL.md /workspace/skills/strict/SKILL.md/child /workspace/not-skills/gamma/SKILL.md"}}}`)
	events := ExtractSkillEvents(record, rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", &model)
	if len(events) != 2 || events[0].SkillName != "alpha" || events[1].SkillName != "beta" {
		t.Fatalf("extracted skill events = %#v", events)
	}
	if events[0].OccurredAtMS != 1780315200000 || events[0].Model == nil || *events[0].Model != model || events[0].RootSessionID != "root" {
		t.Fatalf("skill event metadata = %#v", events[0])
	}
}

func TestExtractSkillEventsJSONArgumentsDeduplicateAndSort(t *testing.T) {
	arguments, err := json.Marshal(map[string]string{"command": "cat /skills/zeta/SKILL.md /skills/alpha/SKILL.md /skills/zeta/SKILL.md"})
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := json.Marshal(string(arguments))
	if err != nil {
		t.Fatal(err)
	}
	record := skillTestRecord(`{"type":"response_item","timestamp":1780315200000,"payload":{"type":"function_call","name":"exec_command","arguments":` + string(encoded) + `}}`)
	events := ExtractSkillEvents(record, rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", nil)
	if len(events) != 2 || events[0].SkillName != "alpha" || events[1].SkillName != "zeta" {
		t.Fatalf("JSON-string command dedupe/order = %#v", events)
	}
}

func TestExtractSkillEventsPluginCacheAndNameLimit(t *testing.T) {
	plugin := skillTestRecord(`{"type":"response_item","payload":{"type":"local_shell_call","timestamp":1780315200000,"action":{"command":["cat /home/.codex/plugins/cache/my-plugin/1.2.0/skills/tool/SKILL.md"]}}}`)
	got := ExtractSkillEvents(plugin, rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", nil)
	if len(got) != 1 || got[0].SkillName != "my-plugin:tool" {
		t.Fatalf("plugin skill = %#v", got)
	}
	tooManyBytes := strings.Repeat("é", 65)
	if validSkillName(strings.Repeat("a", 128)) == false || validSkillName(tooManyBytes) {
		t.Fatal("skill name limit must accept 128 ASCII bytes and reject 130 UTF-8 bytes")
	}
	longSkill := skillTestRecord(`{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":{"cmd":"cat /skills/` + tooManyBytes + `/SKILL.md"},"timestamp":1780315200000}}`)
	if events := ExtractSkillEvents(longSkill, rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", nil); len(events) != 0 {
		t.Fatalf("overlong UTF-8 skill was accepted: %#v", events)
	}
}

func TestExtractSkillEventsCustomExecLexesOnlyRealCalls(t *testing.T) {
	input := `const text = "tools.exec_command({cmd:'/skills/fake/SKILL.md'})";
// tools.exec_command({cmd:'/skills/comment/SKILL.md'})
/* tools.exec_command({cmd:'/skills/block/SKILL.md'}) */
const template = ` + "`tools.exec_command({cmd:'/skills/template/SKILL.md'})`" + `;
tools.exec_command({cmd: '/skills/real/SKILL.md', timeout: 10});`
	encodedInput, err := json.Marshal(input)
	if err != nil {
		t.Fatal(err)
	}
	record := skillTestRecord(`{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","timestamp":1780315200000,"input":` + string(encodedInput) + `}}`)
	events := ExtractSkillEvents(record, rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", nil)
	if len(events) != 1 || events[0].SkillName != "real" {
		t.Fatalf("custom exec skills = %#v", events)
	}
}

func TestExtractSkillEventsRejectsNonOwningAndMalformedRecords(t *testing.T) {
	record := skillTestRecord(`{"type":"response_item","payload":{"type":"function_call","name":"exec_command","timestamp":1780315200000,"arguments":{"cmd":"cat /skills/valid/SKILL.md"}}}`)
	for _, ownership := range []rollout.Ownership{
		{Kind: rollout.OwnershipUnknown, ThreadID: "thread"},
		{Kind: rollout.OwnershipOwning},
	} {
		if events := ExtractSkillEvents(record, ownership, "root", nil); len(events) != 0 {
			t.Fatalf("non-owning record created skill events: %#v", events)
		}
	}
	if events := ExtractSkillEvents(skillTestRecord(`not json SKILL.md`), rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", nil); len(events) != 0 {
		t.Fatalf("malformed call created skill events: %#v", events)
	}
	if events := ExtractSkillEvents(rollout.Record{JSON: []byte(`{"type":"event","payload":{"type":"function_call","name":"exec_command","arguments":{"cmd":"/skills/valid/SKILL.md"}}}`)}, rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", nil); len(events) != 0 {
		t.Fatalf("non-response_item record created skill events: %#v", events)
	}
}

func TestWriteSkillEventsBuildIsIdempotentAndDoesNotBumpRevision(t *testing.T) {
	fixture := newQuarantineTestFixture(t, false)
	events := []SkillEvent{{
		SourceFileID: fixture.memberFileID, Generation: 1, StartOffset: 4, EndOffset: 20,
		OccurredAtMS: 10, ThreadID: quarantineChildID, RootSessionID: quarantineRootID, SkillName: "alpha",
	}}
	beforeRevision := fixture.db.CurrentRevision().DataRevision
	var first, second bool
	if err := fixture.storage.Write(func(tx *source.WriteTx) error {
		var err error
		first, err = WriteSkillEvents(tx, source.UsageTargetBuild, events, 900)
		if err != nil {
			return err
		}
		second, err = WriteSkillEvents(tx, source.UsageTargetBuild, events, 901)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	if !first || second {
		t.Fatalf("skill write visibility changed: first=%v second=%v", first, second)
	}
	if got := fixture.db.CurrentRevision().DataRevision; got != beforeRevision {
		t.Fatalf("Build skill writes changed data_revision from %d to %d", beforeRevision, got)
	}
	var count int
	if err := fixture.storage.PrivateRead(func(reader storage.PrivateReader) error {
		return reader.QueryRow(`SELECT COUNT(*) FROM codex_skill_usage_events WHERE ledger_epoch=? AND source_file_id=? AND skill_name='alpha'`, fixture.buildEpoch, fixture.memberFileID).Scan(&count)
	}); err != nil {
		t.Fatal(err)
	}
	if count != 1 {
		t.Fatalf("stored skill multiplicity = %d", count)
	}
}

func TestExtractSkillEventsCompatibilityFixture(t *testing.T) {
	data, err := os.ReadFile("testdata/compat/skills_calls.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for lineNumber, line := range strings.Split(strings.TrimSpace(string(data)), "\n") {
		events := ExtractSkillEvents(skillTestRecord(line), rollout.Ownership{Kind: rollout.OwnershipOwning, ThreadID: "thread"}, "root", nil)
		if len(events) != 1 {
			t.Fatalf("compat fixture line %d produced %d skill events: %#v", lineNumber+1, len(events), events)
		}
		names = append(names, events[0].SkillName)
	}
	want := []string{"function-compat", "custom-compat", "local-compat"}
	if strings.Join(names, ",") != strings.Join(want, ",") {
		t.Fatalf("compat fixture skill names = %v, want %v", names, want)
	}
}

func skillTestRecord(encoded string) rollout.Record {
	return rollout.Record{SourceFileID: 7, Generation: 3, LogicalStartOffset: 12, LogicalEndOffset: 80, JSON: []byte(encoded)}
}
