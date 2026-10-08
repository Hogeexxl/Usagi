#!/usr/bin/env python3
import argparse
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tools/rust-schema-oracle/Cargo.toml"
FIXTURES = ROOT / "tests/fixtures/storage/legacy"
PROFILES = ROOT / "internal/storage/legacy_profiles"
ASSISTS = {
    "v10-without-metadata-parser-version": ("metadata_parser_version",),
    "v10-without-last-full-import": ("last_full_import_completed_at_ms",),
    "v10-without-both-metadata-columns": (
        "metadata_parser_version", "last_full_import_completed_at_ms"
    ),
}


def oracle(*args):
    subprocess.run(
        ["cargo", "run", "--locked", "--manifest-path", str(MANIFEST), "--", *map(str, args)],
        cwd=ROOT, check=True,
    )


def standard(directory, version):
    directory.mkdir()
    oracle("create-input", "--version", version, "--output", directory / "input.sqlite3")
    oracle("create-expected", "--input", directory / "input.sqlite3",
           "--from-version", version, "--output", directory / "expected-v14.sqlite3")
    oracle("profile", "--db", directory / "input.sqlite3", "--output", directory / "profile.json")


def build(check):
    with tempfile.TemporaryDirectory(prefix="usagi-rust-oracle-") as temporary:
        scratch = Path(temporary)
        for version in range(1, 15):
            name = f"v{version:02d}"
            directory = scratch / name
            standard(directory, version)
            destinations = [FIXTURES / name / "profile.json", PROFILES / f"{name}.json"]
            if check:
                expected = (directory / "profile.json").read_bytes()
                for destination in destinations:
                    if not destination.is_file() or destination.read_bytes() != expected:
                        raise RuntimeError(f"Profile mismatch {name}: {destination}")
                print(f"Profile {name}: PASS", flush=True)
                if version == 14:
                    semantic_fixture = FIXTURES / name / "expected-v14.sqlite3"
                    if (directory / "expected-v14.sqlite3").read_bytes() != semantic_fixture.read_bytes():
                        raise RuntimeError(f"Rust v14 Expected fixture mismatch: {semantic_fixture}")
                    print("Rust v14 Expected: reproducible; quick_check=ok", flush=True)
            else:
                target = FIXTURES / name
                target.mkdir(parents=True, exist_ok=True)
                PROFILES.mkdir(parents=True, exist_ok=True)
                for filename in ("input.sqlite3", "expected-v14.sqlite3", "profile.json"):
                    shutil.copyfile(directory / filename, target / filename)
                shutil.copyfile(directory / "profile.json", destinations[1])
        if check:
            print("Legacy profiles: 14/14 PASS", flush=True)
            return
        for name, columns in ASSISTS.items():
            directory = scratch / name
            directory.mkdir()
            input_db = directory / "input.sqlite3"
            shutil.copyfile(scratch / "v10/input.sqlite3", input_db)
            # These are the three explicit v11 assist inputs, not migration SQL.
            with sqlite3.connect(input_db) as conn:
                for column in columns:
                    conn.execute(f'ALTER TABLE app_meta DROP COLUMN "{column}"')
            oracle("create-expected", "--input", input_db, "--from-version", 10,
                   "--output", directory / "expected-v14.sqlite3")
            target = FIXTURES / name
            target.mkdir(parents=True, exist_ok=True)
            for filename in ("input.sqlite3", "expected-v14.sqlite3"):
                shutil.copyfile(directory / filename, target / filename)
        print("Legacy fixtures: 14 standard + 3 assist variants generated", flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check-profiles", action="store_true")
    args = parser.parse_args()
    try:
        build(args.check_profiles)
    except subprocess.CalledProcessError as error:
        return error.returncode
    except (OSError, RuntimeError, sqlite3.Error) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
