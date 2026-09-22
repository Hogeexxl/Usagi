#!/usr/bin/env python3
"""Build sanitized Antigravity test fixtures and deterministic SQLite WAL trios.

This script extracts minimal schema, numeric field tags, and locator patterns from real samples
in artifacts/Usagi_多终端会话样本.zip and generates:
  1. tests/fixtures/antigravity/standalone/ (conversation_summaries.db, conversations/*.db, annotations/*.pbtxt)
  2. tests/fixtures/antigravity/wal/ (conversations/*.db, .db-wal, .db-shm generated deterministically)
  3. tests/fixtures/antigravity/manifest.json
"""

import json
import os
import shutil
import sqlite3
import tempfile
from pathlib import Path


def encode_varint(val: int) -> bytes:
    res = bytearray()
    while val > 0x7F:
        res.append((val & 0x7F) | 0x80)
        val >>= 7
    res.append(val & 0x7F)
    return bytes(res)


def encode_field(tag: int, wire_type: int, data: bytes | int) -> bytes:
    header = encode_varint((tag << 3) | wire_type)
    if wire_type == 0:
        assert isinstance(data, int)
        return header + encode_varint(data)
    elif wire_type == 2:
        assert isinstance(data, bytes)
        return header + encode_varint(len(data)) + data
    else:
        raise ValueError(f"unsupported wire type {wire_type}")


def build_gen_metadata_data(
    input_tokens: int,
    output_tokens: int,
    cache_read_tokens: int,
    thinking_output_tokens: int,
    response_id: str,
    model: str = "gemini-3.8-flash",
    model_display_name: str | None = None,
) -> bytes:
    usage = (
        encode_field(2, 0, input_tokens)
        + encode_field(3, 0, output_tokens)
        + encode_field(5, 0, cache_read_tokens)
        + encode_field(9, 0, thinking_output_tokens)
        + encode_field(10, 0, max(0, output_tokens - thinking_output_tokens))
        + encode_field(11, 2, response_id.encode("utf-8"))
    )

    chat_model = encode_field(4, 2, usage)
    if model_display_name is not None:
        chat_model += encode_field(18, 2, model_display_name.encode("utf-8"))
    chat_model += encode_field(19, 2, model.encode("utf-8"))

    return encode_field(1, 2, chat_model)


def build_step_payload(response_id: str) -> bytes:
    sub9 = encode_field(11, 2, response_id.encode("utf-8"))
    sub5 = encode_field(9, 2, sub9)
    return encode_field(1, 0, 15) + encode_field(5, 2, sub5)


def build_step_metadata(seconds: int, nanos: int, response_id: str) -> bytes:
    ts = encode_field(1, 0, seconds) + encode_field(2, 0, nanos)
    sub9 = encode_field(11, 2, response_id.encode("utf-8"))
    return (
        encode_field(1, 2, ts)
        + encode_field(3, 0, 2)  # source = 2
        + encode_field(9, 2, sub9)
    )


def build_trajectory_metadata_blob(
    cascade_id: str, workspace_uri: str | None = None
) -> bytes:
    blob = encode_field(6, 2, cascade_id.encode("utf-8"))
    if workspace_uri is not None:
        blob += encode_field(7, 2, workspace_uri.encode("utf-8"))
    else:
        blob += encode_field(18, 2, b"outside-of-project")
    return blob


def create_conversation_db(
    db_path: Path,
    cascade_id: str,
    events: list[dict],
    workspace_uri: str | None = None,
):
    db_path.parent.mkdir(parents=True, exist_ok=True)
    if db_path.exists():
        db_path.unlink()

    conn = sqlite3.connect(db_path)
    c = conn.cursor()

    c.execute(
        """
        CREATE TABLE trajectory_meta (
            trajectory_id TEXT PRIMARY KEY,
            cascade_id TEXT,
            trajectory_type INTEGER,
            source INTEGER
        )
    """
    )

    c.execute(
        """
        CREATE TABLE steps (
            idx INTEGER PRIMARY KEY,
            step_type INTEGER NOT NULL DEFAULT 0,
            status INTEGER NOT NULL DEFAULT 0,
            has_subtrajectory NUMERIC NOT NULL DEFAULT 0,
            metadata BLOB,
            error_details BLOB,
            permissions BLOB,
            task_details BLOB,
            render_info BLOB,
            step_payload BLOB,
            step_format INTEGER NOT NULL DEFAULT 0
        )
    """
    )
    c.execute("CREATE INDEX idx_steps_status ON steps(status)")
    c.execute("CREATE INDEX idx_steps_step_type ON steps(step_type)")

    c.execute(
        """
        CREATE TABLE gen_metadata (
            idx INTEGER PRIMARY KEY,
            data BLOB,
            size INTEGER NOT NULL DEFAULT 0
        )
    """
    )

    c.execute(
        """
        CREATE TABLE trajectory_metadata_blob (
            id TEXT PRIMARY KEY DEFAULT 'main',
            data BLOB
        )
    """
    )

    c.execute(
        """
        CREATE TABLE executor_metadata (
            idx INTEGER PRIMARY KEY,
            data BLOB
        )
    """
    )

    c.execute(
        """
        CREATE TABLE parent_references (
            idx INTEGER PRIMARY KEY,
            data BLOB
        )
    """
    )

    c.execute(
        """
        CREATE TABLE battle_mode_infos (
            idx INTEGER PRIMARY KEY,
            data BLOB
        )
    """
    )

    # Insert trajectory_meta
    c.execute(
        "INSERT INTO trajectory_meta VALUES ('main', ?, 1, 1)",
        (cascade_id,),
    )

    # Insert trajectory_metadata_blob
    tb_data = build_trajectory_metadata_blob(cascade_id, workspace_uri)
    c.execute(
        "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?)",
        (tb_data,),
    )

    # Insert events into gen_metadata and steps
    for idx, ev in enumerate(events):
        gen_data = build_gen_metadata_data(
            input_tokens=ev["input_tokens"],
            output_tokens=ev["output_tokens"],
            cache_read_tokens=ev["cache_read_tokens"],
            thinking_output_tokens=ev["thinking_output_tokens"],
            response_id=ev["response_id"],
            model=ev.get("model", "gemini-3.8-flash"),
            model_display_name=ev.get("model_display_name"),
        )
        c.execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (?, ?, ?)",
            (idx, gen_data, len(gen_data)),
        )

        step_idx = idx * 2 + 1  # non-consecutive idx with step_payload
        step_payload = build_step_payload(ev["response_id"])
        step_metadata = build_step_metadata(
            ev["occurred_seconds"],
            ev["occurred_nanos"],
            ev["response_id"],
        )
        c.execute(
            """
            INSERT INTO steps (idx, step_type, status, has_subtrajectory, metadata, step_payload)
            VALUES (?, 15, 0, 0, ?, ?)
        """,
            (step_idx, step_metadata, step_payload),
        )

    conn.commit()
    conn.close()


def create_summary_db(db_path: Path, summaries: list[dict]):
    db_path.parent.mkdir(parents=True, exist_ok=True)
    if db_path.exists():
        db_path.unlink()

    conn = sqlite3.connect(db_path)
    c = conn.cursor()

    c.execute(
        """
        CREATE TABLE conversation_summaries (
            conversation_id TEXT PRIMARY KEY,
            title TEXT NOT NULL DEFAULT '',
            preview TEXT NOT NULL DEFAULT '',
            step_count INTEGER NOT NULL DEFAULT 0,
            last_modified_time DATETIME NOT NULL,
            workspace_uris TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT '',
            source TEXT NOT NULL DEFAULT '',
            project_id TEXT NOT NULL DEFAULT '',
            agent_name TEXT NOT NULL DEFAULT '',
            parent_conversation_id TEXT NOT NULL DEFAULT '',
            nesting_depth INTEGER NOT NULL DEFAULT 0,
            battle_id TEXT NOT NULL DEFAULT '',
            winning_conversation_id TEXT NOT NULL DEFAULT '',
            not_fully_idle NUMERIC NOT NULL DEFAULT 0,
            killed NUMERIC NOT NULL DEFAULT 0,
            last_user_input_time DATETIME NOT NULL DEFAULT '',
            last_user_input_step_index INTEGER NOT NULL DEFAULT -1,
            app_data_dir TEXT NOT NULL DEFAULT '',
            raw_summary BLOB,
            group_id TEXT NOT NULL DEFAULT ''
        )
    """
    )
    c.execute(
        "CREATE INDEX idx_conversation_summaries_last_modified_time ON conversation_summaries(last_modified_time)"
    )

    for s in summaries:
        c.execute(
            """
            INSERT INTO conversation_summaries (
                conversation_id, title, step_count, last_modified_time, workspace_uris
            ) VALUES (?, ?, ?, ?, ?)
        """,
            (
                s["conversation_id"],
                s["title"],
                s.get("step_count", 10),
                s["last_modified_time"],
                s.get("workspace_uris", ""),
            ),
        )

    conn.commit()
    conn.close()


def build_wal_trio(
    target_dir: Path,
    cascade_id: str,
    base_events: list[dict],
    wal_events: list[dict],
    workspace_uri: str | None = None,
):
    """Deterministically create .db, .db-wal, and .db-shm with uncheckpointed commits."""
    target_dir.mkdir(parents=True, exist_ok=True)
    db_file = target_dir / f"{cascade_id}.db"
    wal_file = target_dir / f"{cascade_id}.db-wal"
    shm_file = target_dir / f"{cascade_id}.db-shm"

    for p in [db_file, wal_file, shm_file]:
        if p.exists():
            p.unlink()

    with tempfile.TemporaryDirectory() as td:
        temp_db = Path(td) / "wal_temp.db"
        create_conversation_db(temp_db, cascade_id, base_events, workspace_uri)

        # Connect writer and enable WAL with autocheckpoint=0
        writer = sqlite3.connect(temp_db)
        writer.execute("PRAGMA journal_mode=WAL")
        writer.execute("PRAGMA wal_autocheckpoint=0")
        writer.commit()

        # Connect reader and start deferred transaction to hold shared lock on WAL
        reader = sqlite3.connect(temp_db)
        reader.execute("BEGIN DEFERRED")
        reader.execute("SELECT count(*) FROM steps").fetchall()

        # Write additional synthetic events into WAL
        base_count = len(base_events)
        for i, ev in enumerate(wal_events):
            idx = base_count + i
            gen_data = build_gen_metadata_data(
                input_tokens=ev["input_tokens"],
                output_tokens=ev["output_tokens"],
                cache_read_tokens=ev["cache_read_tokens"],
                thinking_output_tokens=ev["thinking_output_tokens"],
                response_id=ev["response_id"],
                model=ev.get("model", "gemini-3.8-flash"),
            )
            writer.execute(
                "INSERT INTO gen_metadata (idx, data, size) VALUES (?, ?, ?)",
                (idx, gen_data, len(gen_data)),
            )
            step_idx = idx * 2 + 1
            step_payload = build_step_payload(ev["response_id"])
            step_metadata = build_step_metadata(
                ev["occurred_seconds"],
                ev["occurred_nanos"],
                ev["response_id"],
            )
            writer.execute(
                """
                INSERT INTO steps (idx, step_type, status, has_subtrajectory, metadata, step_payload)
                VALUES (?, 15, 0, 0, ?, ?)
            """,
                (step_idx, step_metadata, step_payload),
            )
        writer.commit()

        # Copy the trio while reader transaction holds WAL snapshot
        shutil.copy2(temp_db, db_file)
        shutil.copy2(temp_db.with_name("wal_temp.db-wal"), wal_file)
        shutil.copy2(temp_db.with_name("wal_temp.db-shm"), shm_file)

        reader.close()
        writer.close()


def main():
    repo_root = Path(__file__).resolve().parent.parent.parent
    fixtures_dir = repo_root / "tests" / "fixtures" / "antigravity"

    standalone_dir = fixtures_dir / "standalone"
    wal_dir = fixtures_dir / "wal"

    effa_id = "effa6389-921a-497e-87e0-5a2962526c07"
    active_wal_id = "49e69e84-d3f2-4f56-8c17-4ea8ded58b31"

    effa_events = [
        {
            "response_id": "JZyoarzvF-ulqfkPk-CLkQc",
            "input_tokens": 1318,
            "output_tokens": 363,
            "cache_read_tokens": 24,
            "thinking_output_tokens": 63,
            "model": "gemini-3.8-flash",
            "occurred_seconds": 1789434916,
            "occurred_nanos": 203160000,
        },
        {
            "response_id": "K5yoataiN9aug8UPruDiyQY",
            "input_tokens": 1500,
            "output_tokens": 400,
            "cache_read_tokens": 50,
            "thinking_output_tokens": 80,
            "model": "gemini-3.8-flash",
            "occurred_seconds": 1789434923,
            "occurred_nanos": 141330000,
        },
        {
            "response_id": "L5yoasiYKIeDvr0P6rmsiQM",
            "input_tokens": 2000,
            "output_tokens": 500,
            "cache_read_tokens": 100,
            "thinking_output_tokens": 120,
            "model": "gemini-3.8-flash",
            "occurred_seconds": 1789434930,
            "occurred_nanos": 500000000,
        },
    ]

    wal_base_events = [
        {
            "response_id": "0ROuauGOBOm5vr0PzsT8uAY",
            "input_tokens": 1000,
            "output_tokens": 200,
            "cache_read_tokens": 0,
            "thinking_output_tokens": 50,
            "model": "gemini-3.8-flash",
            "occurred_seconds": 1789435000,
            "occurred_nanos": 100000000,
        }
    ]

    wal_incremental_events = [
        {
            "response_id": "1BOuaoqBLae_vr0Pie78sAM",
            "input_tokens": 1200,
            "output_tokens": 250,
            "cache_read_tokens": 30,
            "thinking_output_tokens": 60,
            "model": "gemini-3.8-flash",
            "occurred_seconds": 1789435010,
            "occurred_nanos": 200000000,
        }
    ]

    # 1. Standalone fixture
    create_conversation_db(
        standalone_dir / "conversations" / f"{effa_id}.db",
        effa_id,
        effa_events,
        workspace_uri="file:///Users/hogee/Desktop/Usagi",
    )

    create_summary_db(
        standalone_dir / "conversation_summaries.db",
        [
            {
                "conversation_id": effa_id,
                "title": "Sanitized Standalone Session",
                "step_count": 199,
                "last_modified_time": "2026-09-15 03:46:33.54883+00:00",
                "workspace_uris": '["file:///Users/hogee/Desktop/Usagi"]',
            }
        ],
    )

    annotations_dir = standalone_dir / "annotations"
    annotations_dir.mkdir(parents=True, exist_ok=True)
    with open(annotations_dir / f"{effa_id}.pbtxt", "w", encoding="utf-8") as f:
        f.write(
            'title:"Sanitized Standalone Session Annotation"\n'
            "last_user_view_time:{seconds:1789434930 nanos:500000000}\n"
        )

    # 2. Deterministic WAL fixture
    build_wal_trio(
        wal_dir / "conversations",
        active_wal_id,
        wal_base_events,
        wal_incremental_events,
        workspace_uri="file:///Users/hogee/Desktop/Usagi",
    )

    create_summary_db(
        wal_dir / "conversation_summaries.db",
        [
            {
                "conversation_id": active_wal_id,
                "title": "Sanitized WAL Active Session",
                "step_count": 10,
                "last_modified_time": "2026-09-15 04:00:00.00000+00:00",
                "workspace_uris": '["file:///Users/hogee/Desktop/Usagi"]',
            }
        ],
    )

    # 3. Manifest
    manifest = {
        "format_version": 1,
        "source": "antigravity",
        "description": "Sanitized Antigravity test fixtures generated from schema and samples",
        "fixtures": {
            "standalone": {
                "conversation_id": effa_id,
                "db_file": f"standalone/conversations/{effa_id}.db",
                "summaries_file": "standalone/conversation_summaries.db",
                "annotation_file": f"standalone/annotations/{effa_id}.pbtxt",
                "expected_events_count": len(effa_events),
                "workspace_uri": "file:///Users/hogee/Desktop/Usagi",
                "title": "Sanitized Standalone Session",
                "events": effa_events,
            },
            "wal": {
                "conversation_id": active_wal_id,
                "db_file": f"wal/conversations/{active_wal_id}.db",
                "wal_file": f"wal/conversations/{active_wal_id}.db-wal",
                "shm_file": f"wal/conversations/{active_wal_id}.db-shm",
                "summaries_file": "wal/conversation_summaries.db",
                "expected_events_count": len(wal_base_events)
                + len(wal_incremental_events),
                "base_events": wal_base_events,
                "wal_events": wal_incremental_events,
            },
        },
        "schema_contracts": {
            "gen_metadata": {
                "columns": ["idx", "data", "size"],
                "proto_path": "chatModel(1) -> usage(4)",
                "fields": {
                    "input_tokens": 2,
                    "output_tokens": 3,
                    "cache_read_tokens": 5,
                    "thinking_output_tokens": 9,
                    "other_output_tokens": 10,
                    "response_id": 11,
                    "model_display_name": 18,
                    "response_model": 19,
                },
            },
            "steps": {
                "columns": ["idx", "step_type", "metadata", "step_payload"],
                "join_step_type": 15,
                "payload_response_id_path": "step_payload -> 5 -> 9 -> 11",
                "metadata_timestamp_path": "metadata -> 1 -> (seconds=1, nanos=2)",
                "metadata_source_tag": 3,
                "metadata_expected_source": 2,
            },
            "trajectory_metadata_blob": {
                "columns": ["id", "data"],
                "workspace_uri_tag": 7,
            },
        },
    }

    manifest_path = fixtures_dir / "manifest.json"
    with open(manifest_path, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)

    print(f"Successfully generated Antigravity fixtures in {fixtures_dir}")


if __name__ == "__main__":
    main()
