#!/usr/bin/env python3
"""Reproducible, isolated search-first ams/session-migrate benchmark (stdlib only).

Build/install both tools first, then pass executable paths, not shell commands:
  python3 scripts/benchmark.py --ams target/release/ams \
      --baseline /tmp/session-migrate-venv/bin/session-migrate \
      --baseline-repo /tmp/ams-session-migrate --output benchmark.json

--migration additionally checks and times a >=16 MiB OMP -> Pi dry run.
No benchmark touches the invoking user's agent homes or drops filesystem caches.
The baseline executable must be installed from the pinned checkout below.

Fixture structures are derived from session-migrate's sanitized native fixtures:
MIT License
Copyright (c) 2026 xhluca
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:
The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.
THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import uuid


BASELINE_REVISION = "45351ff95ce79684c23377920b92026ecbe9759b"
FIXTURES = {
    "omp": "tests/fixtures/omp-18.0.5/basic.jsonl",
    "pi": "tests/fixtures/pi-0.80.6/basic.jsonl",
    "codex": "tests/fixtures/codex-0.144.4/basic.jsonl",
}
TITLE_QUERY = "atlasgarden"
BODY_QUERY = "buriedlantern"
TIMESTAMP = "2026-08-25T12:00:00Z"
NAMESPACE = uuid.UUID("15f1938c-798d-424a-80db-cefbb0c72ae1")


class BenchmarkError(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise BenchmarkError(message)


def positive(value: str) -> int:
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def executable(value: str) -> str:
    candidate = shutil.which(value)
    require(candidate is not None, f"executable not found: {value}")
    # Do not resolve symlinks: a venv entry point's location is meaningful.
    return os.path.abspath(candidate)


def run(command: list[str], env: dict[str, str], cwd: Path, *, timed: bool = False,
        as_json: bool = True) -> tuple[object, float]:
    """Exit checking and JSON parsing are outside the measured child lifetime."""
    with tempfile.TemporaryFile() as errors:
        start = time.perf_counter()
        result = subprocess.run(
            command, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL if timed else subprocess.PIPE,
            stderr=errors, check=False,
        )
        elapsed = time.perf_counter() - start
        if result.returncode != 0:
            errors.seek(0)
            detail = errors.read(16_384).decode("utf-8", errors="replace")
            raise BenchmarkError(
                f"exit {result.returncode}: {json.dumps(command)}\n{detail}"
            )
        if timed:
            return None, elapsed
        text = result.stdout.decode("utf-8")
        if not as_json:
            return text.strip(), elapsed
        try:
            return json.loads(text), elapsed
        except (UnicodeError, ValueError) as exc:
            raise BenchmarkError(f"invalid JSON from {command!r}: {text[:1000]}") from exc


def isolated_env(home: Path) -> dict[str, str]:
    # Retain executable lookup/dynamic loader/venv settings, but no native roots,
    # user configuration paths, or provider credentials from the invoking shell.
    retained = ("PATH", "SYSTEMROOT", "LD_LIBRARY_PATH", "VIRTUAL_ENV")
    env = {key: os.environ[key] for key in retained if key in os.environ}
    env.update({
        "HOME": str(home), "USERPROFILE": str(home),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "XDG_STATE_HOME": str(home / "state"),
        "XDG_CACHE_HOME": str(home / "cache"),
        "TMPDIR": str(home / "tmp"), "LC_ALL": "C", "TZ": "UTC",
        "PYTHONHASHSEED": "0", "PYTHONNOUSERSITE": "1", "NO_COLOR": "1",
    })
    for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                "XDG_CACHE_HOME", "TMPDIR"):
        Path(env[key]).mkdir(parents=True, exist_ok=True)
    return env


def jsonl(path: Path, records: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as stream:
        for record in records:
            if record.get("type") == "title":
                # Native OMP's mutable first-line title slot includes its newline.
                record["pad"] = ""
                unpadded = json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n"
                require(len(unpadded.encode("utf-8")) <= 256, "OMP fixture title exceeds native slot")
                record["pad"] = " " * (256 - len(unpadded.encode("utf-8")))
            stream.write(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n")
    os.utime(path, (1787659200, 1787659200))


def message(identifier: str, parent: str | None, role: str, text: str) -> dict:
    body = {"role": role, "content": [{"type": "text", "text": text}],
            "timestamp": 1787659200000}
    if role == "assistant":
        body.update({"api": "openai-completions", "provider": "fixture",
                     "model": "fixture-model", "stopReason": "stop",
                     "usage": {"input": 0, "output": 0, "cacheRead": 0,
                               "cacheWrite": 0, "totalTokens": 0,
                               "cost": {"input": 0, "output": 0, "cacheRead": 0,
                                        "cacheWrite": 0, "total": 0}}})
    return {"type": "message", "id": identifier, "parentId": parent,
            "timestamp": TIMESTAMP, "message": body}


def corpus(repo: Path, root: Path, count: int) -> tuple[dict, dict, list[Path]]:
    templates = {}
    provenance = {}
    for fmt, relative in FIXTURES.items():
        data = (repo / relative).read_bytes()
        templates[fmt] = [json.loads(line) for line in data.splitlines() if line.strip()]
        provenance[fmt] = {"path": relative, "sha256": hashlib.sha256(data).hexdigest()}
    homes = {fmt: root / fmt for fmt in FIXTURES}
    for home in homes.values():
        (home / "sessions").mkdir(parents=True)
    expected = {"all": [], "titles": [], "bodies": [], "by_format": {fmt: 0 for fmt in FIXTURES}}
    files = []
    for index in range(count):
        fmt = list(FIXTURES)[index % len(FIXTURES)]
        identity = str(uuid.uuid5(NAMESPACE, f"session-{index:08d}"))
        title = f"{'atlasgarden' if index % 5 == 0 else 'cedararchive'} session {index:08d}"
        body = (f"Deterministic synthetic discussion {index:08d}. "
                "Map the orchard paths and explain the reproducible archive. ") * 8
        late = (f"The final assistant remembers {BODY_QUERY}." if index % 7 == 0
                else "The final assistant remembers riverstones.")
        records = copy.deepcopy(templates[fmt])
        if fmt in ("omp", "pi"):
            for record in records:
                if record.get("type") == "session":
                    record.update(id=identity, cwd="/synthetic/benchmark", title=title)
                if record.get("type") == "title":
                    record["title"] = title
                    record["pad"] = " " * 128
                if record.get("type") == "session_info":
                    record["name"] = title
                msg = record.get("message", {})
                if msg.get("role") in ("user", "assistant"):
                    if isinstance(msg.get("content"), str):
                        msg["content"] = body
                    else:
                        for block in msg.get("content", []):
                            if block.get("type") == "text":
                                block["text"] = body
            parent = records[-1].get("id")
            records.append(message("benchmark-final", parent, "assistant", late))
        else:
            for record in records:
                payload = record.get("payload", {})
                if record.get("type") == "session_meta":
                    payload.update(id=identity, session_id=identity, cwd="/synthetic/benchmark")
                if payload.get("type") in ("user_message", "agent_message"):
                    payload["message"] = body
                if payload.get("type") == "message" and payload.get("role") in ("user", "assistant"):
                    for block in payload.get("content", []):
                        if block.get("type") in ("input_text", "output_text"):
                            block["text"] = body
            records += [
                {"timestamp": TIMESTAMP, "type": "event_msg",
                 "payload": {"type": "thread_name_updated", "name": title}},
                {"timestamp": TIMESTAMP, "type": "response_item",
                 "payload": {"type": "message", "role": "assistant",
                             "content": [{"type": "output_text", "text": late}]}},
            ]
        path = homes[fmt] / "sessions" / "synthetic" / f"2026-08-25T12-00-00_{identity}.jsonl"
        jsonl(path, records)
        files.append(path)
        expected["all"].append(identity)
        expected["by_format"][fmt] += 1
        if index % 5 == 0:
            expected["titles"].append(identity)
        if index % 7 == 0:
            expected["bodies"].append(identity)
    expected["fixtures"] = provenance
    return homes, expected, files


def snapshot(files: list[Path], root: Path) -> dict:
    digest = hashlib.sha256()
    size = 0
    for path in sorted(files):
        data = path.read_bytes()
        size += len(data)
        digest.update(str(path.relative_to(root)).encode() + b"\0")
        digest.update(hashlib.sha256(data).digest())
    return {"files": len(files), "bytes": size, "sha256": digest.hexdigest()}


def rows_match(value: object, expected: list[str], tool: str) -> None:
    require(isinstance(value, list), f"{tool}: expected JSON row array")
    ids = [row.get("native_id") if tool == "ams" else row.get("session_id") for row in value]
    require(len(ids) == len(expected) and set(ids) == set(expected),
            f"{tool}: matched session IDs differ; expected {len(expected)}, got {len(ids)}")
    bad = [row for row in value if row.get("error") or row.get("status") in
           ("corrupt", "missing", "error", "unsupported", "invalid")]
    require(not bad, f"{tool}: unhealthy catalog entries: {bad[:3]}")


def stats(samples: list[float]) -> dict:
    if not samples:
        return {"successful_samples": 0}
    ordered = sorted(samples)
    # Nearest-rank p95; no interpolation that invents an unobserved sample.
    return {"successful_samples": len(samples), "seconds": samples,
            "median_seconds": statistics.median(samples),
            "p95_seconds": ordered[math.ceil(len(ordered) * .95) - 1],
            "min_seconds": ordered[0], "max_seconds": ordered[-1]}


class Harness:
    def __init__(self, args, root: Path, report: dict):
        self.args, self.root, self.report = args, root, report
        self.envs = {tool: isolated_env(root / f"{tool}-home") for tool in ("ams", "baseline")}
        self.bins = {"ams": executable(args.ams), "baseline": executable(args.baseline)}
        self.homes, self.expected, self.files = corpus(args.baseline_repo, root / "corpus", args.sessions)
        self.before = snapshot(self.files, root / "corpus")
        report["corpus"] = dict(self.before, formats=self.expected["by_format"],
                                fixture_templates=self.expected["fixtures"],
                                title_query=TITLE_QUERY, title_matches=len(self.expected["titles"]),
                                body_query=BODY_QUERY, body_matches=len(self.expected["bodies"]))
        self.limit = str(args.sessions + 1)
        self.warm = {tool: root / f"{tool}-warm.sqlite3" for tool in self.bins}

    def command(self, tool: str, db: Path, operation: str) -> list[str]:
        if tool == "ams":
            base = [self.bins[tool], "--db", str(db), "--json", "--jobs", str(self.args.jobs)]
            if operation == "scan":
                return base + ["scan", "--no-defaults"] + [
                    item for fmt, home in self.homes.items()
                    for item in ("--root", f"{fmt}={home / 'sessions'}")]
            if operation == "list":
                return base + ["ls", "-n", self.limit]
            if operation == "titles":
                return base + ["find", TITLE_QUERY, "--titles-only", "-n", self.limit]
            return base + ["find", BODY_QUERY, "-n", self.limit]
        base = [self.bins[tool], "--catalog", str(db), "catalog"]
        if operation == "scan":
            return base + ["refresh", "--no-auto-roots", "--json"] + [
                item for fmt, home in self.homes.items() for item in (f"--{fmt}-root", str(home))]
        if operation == "list":
            return base + ["list", "--json", "--limit", self.limit]
        return base + ["search", TITLE_QUERY, "--json", "--limit", self.limit]

    def call(self, tool: str, command: list[str], **kwargs) -> tuple[object, float]:
        return run(command, self.envs[tool], self.root, **kwargs)

    def validate_scan(self, tool: str, db: Path, *, fresh: bool) -> None:
        value, _ = self.call(tool, self.command(tool, db, "scan"))
        require(isinstance(value, dict), f"{tool}: scan did not return an object")
        require(value.get("files_seen") == self.args.sessions, f"{tool}: wrong files_seen: {value}")
        require(value.get("errors", 0) == 0 and not value.get("root_errors", 0),
                f"{tool}: scan errors: {value}")
        counter = "scanned" if fresh else "unchanged"
        require(value.get(counter) == self.args.sessions, f"{tool}: wrong {counter}: {value}")
        rows, _ = self.call(tool, self.command(tool, db, "list"))
        rows_match(rows, self.expected["all"], tool)

    def preflight(self) -> None:
        versions = {}
        for tool in self.bins:
            version, _ = self.call(tool, [self.bins[tool], "--version"], as_json=False)
            versions[tool] = {"version": version, "executable": self.bins[tool],
                              "sha256": hashlib.sha256(Path(self.bins[tool]).read_bytes()).hexdigest()}
            self.validate_scan(tool, self.warm[tool], fresh=True)
            self.validate_scan(tool, self.warm[tool], fresh=False)
            rows, _ = self.call(tool, self.command(tool, self.warm[tool], "titles"))
            rows_match(rows, self.expected["titles"], tool)
        self.report["executables"] = versions
        rows, _ = self.call("ams", self.command("ams", self.warm["ams"], "bodies"))
        rows_match(rows, self.expected["bodies"], "ams")
        self.report["correctness"] = {"preflight": "passed", "metadata_matched_ids": True,
                                       "body_matched_ids": True, "incremental_counts": True}

    def record(self, name: str, tools: tuple[str, ...], prepare, validate, *, comparable: bool) -> None:
        samples = {tool: [] for tool in tools}
        metric = {"comparable": comparable, "runs": [], "implementations": {}}
        self.report["measurements"][name] = metric
        for repeat in range(self.args.repeats):
            order = tools if repeat % 2 == 0 else tuple(reversed(tools))
            for tool in order:
                command, state = prepare(tool, repeat)
                try:
                    _, elapsed = self.call(tool, command, timed=True)
                    validate(tool, command, state)
                except Exception as exc:
                    metric["runs"].append({"repeat": repeat, "tool": tool, "status": "failed",
                                           "command": command, "error": str(exc)})
                    raise
                samples[tool].append(elapsed)
                metric["runs"].append({"repeat": repeat, "tool": tool, "status": "ok",
                                       "command": command, "seconds": elapsed})
                metric["implementations"][tool] = stats(samples[tool])
        if comparable and all(samples.values()):
            metric["baseline_over_ams_median_ratio"] = (
                statistics.median(samples["baseline"]) / statistics.median(samples["ams"]))

    def benchmark(self) -> None:
        both = ("ams", "baseline")
        self.record("cli_startup_help", both,
                    lambda tool, _: ([self.bins[tool], "--help"], None),
                    lambda *_: None, comparable=True)

        def fresh(tool, repeat):
            db = self.root / f"{tool}-fresh-{repeat}.sqlite3"
            require(not db.exists(), f"fresh database already exists: {db}")
            return self.command(tool, db, "scan"), db

        def checked_index(tool, command, db):
            # Check the timed scan's catalog BEFORE another scan can repair it.
            rows, _ = self.call(tool, self.command(tool, db, "list"))
            rows_match(rows, self.expected["all"], tool)
            # Re-run outside timing to assert the whole index is incremental.
            self.validate_scan(tool, db, fresh=False)

        self.record("fresh_database_scan_warm_filesystem", both, fresh, checked_index,
                    comparable=True)
        self.record("warm_incremental_scan", both,
                    lambda tool, _: (self.command(tool, self.warm[tool], "scan"), self.warm[tool]),
                    checked_index, comparable=True)

        def checked_query(tool, command, expected):
            rows, _ = self.call(tool, command)
            rows_match(rows, expected, tool)

        self.record("metadata_title_search", both,
                    lambda tool, _: (self.command(tool, self.warm[tool], "titles"), self.expected["titles"]),
                    checked_query, comparable=True)
        self.record("conversation_body_search_ams_only", ("ams",),
                    lambda tool, _: (self.command(tool, self.warm[tool], "bodies"), self.expected["bodies"]),
                    checked_query, comparable=False)

    def migration(self) -> None:
        source = self.root / "large-source.jsonl"
        text = "Deterministic synthetic migration body orchard archive.\n" * 320_000
        records = [{"type": "title", "v": 1, "title": "Synthetic large migration",
                    "source": "user", "updatedAt": TIMESTAMP, "pad": ""},
                   {"type": "session", "version": 3,
                    "id": str(uuid.uuid5(NAMESPACE, "large-source")),
                    "timestamp": TIMESTAMP, "cwd": "/synthetic/benchmark",
                    "title": "Synthetic large migration", "titleSource": "user"},
                   message("large-user", None, "user", text),
                   message("large-answer", "large-user", "assistant", "Synthetic complete.")]
        jsonl(source, records)
        require(source.stat().st_size >= 16 * 1024 * 1024, "large source is smaller than 16 MiB")
        source_hash = hashlib.sha256(source.read_bytes()).hexdigest()

        def command(tool, home, dry):
            if tool == "ams":
                result = [self.bins[tool], "--json", "transfer", str(source), "--from", "omp",
                          "--to", "pi", "--home", str(home), "--cwd", "/synthetic/benchmark"]
            else:
                result = [self.bins[tool], "import", str(source), "--format", "omp", "--to", "pi",
                          "--home", str(home), "--cwd", "/synthetic/benchmark"]
            return result + (["--dry-run"] if dry else [])

        # Before any migration timings, create real native outputs in disposable
        # target homes and make the baseline's parser re-read BOTH writers' Pi.
        for tool in ("ams", "baseline"):
            target = self.root / f"{tool}-migration-check"
            self.call(tool, command(tool, target, False))
            native = sorted(target.rglob("*.jsonl"))
            require(len(native) == 1, f"{tool}: expected exactly one native migration output")
            parsed = self.root / f"{tool}-baseline-reparsed.jsonl"
            self.call("baseline", [self.bins["baseline"], "convert", str(native[0]), "--format", "pi",
                                   "--to", "pi", "--output", str(parsed)])
            texts = []
            for line in parsed.read_text(encoding="utf-8").splitlines():
                msg = json.loads(line).get("message", {})
                if msg.get("role") not in ("user", "assistant"):
                    continue
                content = msg.get("content", [])
                value = content if isinstance(content, str) else "".join(
                    block["text"] for block in content if block.get("type") == "text")
                texts.append((msg["role"], value))
            require(texts == [("user", text), ("assistant", "Synthetic complete.")],
                    f"{tool}: baseline-reparsed migration lost or changed conversation text")

        def prepare(tool, repeat):
            home = self.root / f"{tool}-dry-run-{repeat}"
            return command(tool, home, True), home

        def validate(tool, cmd, home):
            result, _ = self.call(tool, cmd)
            if isinstance(result, list):
                require(len(result) == 1, "unexpected migration result count")
                result = result[0]
            require(isinstance(result, dict) and result.get("dry_run") is True,
                    f"{tool}: missing dry_run manifest")
            require(not home.exists() or not any(home.rglob("*")), f"{tool}: dry run wrote target files")
            require(hashlib.sha256(source.read_bytes()).hexdigest() == source_hash,
                    f"{tool}: changed source transcript")

        self.report["migration"] = {"source_bytes": source.stat().st_size,
                                     "source_sha256": source_hash, "from": "omp", "to": "pi",
                                     "baseline_reparsed_both_outputs": True,
                                     "exact_conversation_text_preserved": True}
        self.record("optional_large_source_dry_run_omp_to_pi", ("ams", "baseline"),
                    prepare, validate, comparable=True)


def markdown(report: dict) -> str:
    lines = ["# ams benchmark", "", f"Status: {report['status']}",
             f"Baseline revision: `{report['baseline']['revision']}`", "",
             "Fresh database is **not** cold filesystem cache. All corpus files are generated",
             "and read before timing. Body search is an ams-only feature, not a speedup.", "",
             "| Operation | Tool | Samples | Median ms | p95 ms | Min ms | Max ms |",
             "|---|---|---:|---:|---:|---:|---:|"]
    for name, measurement in report["measurements"].items():
        for tool, summary in measurement["implementations"].items():
            values = [f"{summary[key] * 1000:.3f}" for key in
                      ("median_seconds", "p95_seconds", "min_seconds", "max_seconds")]
            lines.append(f"| {name} | {tool} | {summary['successful_samples']} | " + " | ".join(values) + " |")
    if report.get("failures"):
        lines += ["", "Failures (no failed execution is counted as a successful timing):", "",
                  "```", *report["failures"], "```"]
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__.split("Fixture structures", 1)[0].strip(),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--ams", required=True, help="ams executable path")
    parser.add_argument("--baseline", required=True, help="installed session-migrate executable, not a shell command")
    parser.add_argument("--baseline-repo", type=Path, default=Path("/tmp/ams-session-migrate"))
    parser.add_argument("--output", type=Path, default=Path("benchmark.json"))
    parser.add_argument("--sessions", type=positive, default=100, help="total sessions across OMP/Pi/Codex")
    parser.add_argument("--repeats", type=positive, default=7)
    parser.add_argument("--jobs", type=positive, default=1, help="ams worker count; baseline has no jobs option")
    parser.add_argument("--migration", action="store_true", help="also validate and time >=16 MiB OMP -> Pi dry runs")
    parser.add_argument("--markdown", type=Path, help="optional human-readable report path")
    args = parser.parse_args()
    args.baseline_repo = args.baseline_repo.expanduser().absolute()
    report = {
        "schema_version": 1, "status": "failed", "baseline": {"revision": BASELINE_REVISION},
        "configuration": {"sessions": args.sessions, "repeats": args.repeats, "ams_jobs": args.jobs,
                          "baseline_jobs": "not configurable", "migration": args.migration},
        "machine": {"platform": platform.platform(), "architecture": platform.machine(),
                    "processor": platform.processor(), "logical_cpus": os.cpu_count(),
                    "python": platform.python_version()},
        "methodology": {
            "timer": "time.perf_counter; subprocess wall-clock including CLI startup",
            "order": "ams then baseline on even repetitions, baseline then ams on odd repetitions",
            "p95": "nearest-rank", "timed_stdout": "DEVNULL; stderr retained on failure",
            "correctness": "untimed JSON preflight and identical post-timing command checks; exact native ID sets",
            "filesystem_cache": "not dropped; warm/generated corpus, even for fresh database scans",
            "fresh_database": "unique nonexistent SQLite path per implementation per repetition",
            "warm_database": "preflight-indexed, unchanged source corpus",
            "comparison": "metadata: ams find QUERY --titles-only vs baseline catalog search QUERY",
            "indexing_scope": "same source corpus; ams additionally indexes readable conversation text, baseline indexes metadata only",
            "body_search": "ams-only additional feature; no baseline ratio",
            "baseline_executable_requirement": "must be installed from the pinned checkout; executable version and digest recorded",
            "peak_rss": "not measured; cumulative child resource counters are not per-command peak RSS",
            "isolation": "temporary HOME/XDG/TMPDIR, explicit roots only, no inherited native root or credential variables",
        },
        "measurements": {}, "failures": [],
    }
    if not report["machine"]["processor"]:
        try:
            report["machine"]["processor"] = next(
                line.split(":", 1)[1].strip()
                for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")
            )
        except (OSError, StopIteration):
            report["machine"]["processor"] = "unknown"
    try:
        revision = subprocess.run(["git", "-C", str(args.baseline_repo), "rev-parse", "HEAD"],
                                  check=True, capture_output=True, text=True).stdout.strip()
        report["baseline"].update(revision=revision, repository=str(args.baseline_repo),
                                 expected_revision=BASELINE_REVISION)
        require(revision == BASELINE_REVISION,
                f"baseline checkout must be pinned to {BASELINE_REVISION}, got {revision}")
        with tempfile.TemporaryDirectory(prefix="ams-benchmark-") as temporary:
            harness = Harness(args, Path(temporary), report)
            try:
                harness.preflight()
                if args.migration:
                    # Migration correctness must pass before any timed operations.
                    # migration() performs its own correctness before its timings.
                    harness.migration()
                harness.benchmark()
            finally:
                after = snapshot(harness.files, harness.root / "corpus")
                report.setdefault("correctness", {})["source_corpus_unchanged"] = after == harness.before
                require(after == harness.before, "benchmark modified source corpus")
        report["status"] = "passed"
    except Exception as exc:
        report["failures"].append(f"{type(exc).__name__}: {exc}")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if args.markdown:
        args.markdown.parent.mkdir(parents=True, exist_ok=True)
        args.markdown.write_text(markdown(report), encoding="utf-8")
    print(json.dumps({"status": report["status"], "output": str(args.output),
                      "failures": report["failures"]}, sort_keys=True))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
