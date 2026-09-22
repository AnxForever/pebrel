"""Boot Pebrel with explicit `-e` commands and dump what each pane renders.

Separates "the shell hangs at startup" from "the PTY read path is dead":
cmd, PowerShell 5.1 and pwsh are launched through the same ConPTY and
environment path as the default tab, and every pane is read for 15s.

The `cmd-env` case captures the exact environment block Pebrel hands its
children; PowerShell 5.1 is then replayed outside the application with that
block and/or that working directory, and the differing variables are bisected
when the block alone reproduces a silent start.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, os.fspath(Path(__file__).resolve().parent.parent))
sys.path.insert(0, os.fspath(Path(__file__).resolve().parent))
import conpty_smoke  # noqa: E402
from conformance.harness import RuntimeClient  # noqa: E402

MARKER = "PEBREL_SMOKE_OK"
POWERSHELL_51 = ["powershell.exe", "-NoLogo", "-NoProfile", "-NoExit", "-Command", f"Write-Output {MARKER}"]
REPLAY_WAIT = 10.0

app = Path(sys.argv[1]).resolve()
output = Path(sys.argv[2])
runtime_library = app.parent / "runtime" / "conpty.dll"
report: dict[str, object] = {}
app_env: dict[str, str] | None = None
app_cwd: str | None = None


def boot(name: str, command: list[str], env_dump: Path | None = None) -> dict[str, object]:
    global app_env
    with tempfile.TemporaryDirectory(prefix="pebrel-cmd-", ignore_cleanup_errors=True) as root:
        config = Path(root) / "config"
        work = Path(root) / "work"
        config.mkdir()
        work.mkdir()
        (config / "pebrel_settings.txt").write_text(
            "keep_session=0\ntray=0\nrestore_session=0\nresume_ai=0\nfetch=0\n"
            "auto_check_updates=0\nai_hooks=0\nwindowing_behavior=use_new\n",
            encoding="utf-8",
        )
        if env_dump is not None:
            env_dump = Path(root) / env_dump
            command = [*command, f"set > {env_dump}"]
        env = dict(os.environ, PEBREL_CONFIG_DIR=os.fspath(config), NEBULA_CONFIG_DIR=os.fspath(config), NEBULA_BOOT_TRACE="1")
        env.pop("PEBREL_RUNTIME_ENDPOINT", None)
        log = open(Path(root) / "app.log", "wb")
        process = subprocess.Popen(
            [os.fspath(app), "--working-directory", os.fspath(work), "-e", *command],
            cwd=work, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
        )
        entry: dict[str, object] = {"command": command}
        port = config / "runtime.port"
        deadline = time.monotonic() + 20
        client = None
        while time.monotonic() < deadline and client is None:
            if port.is_file():
                try:
                    client = RuntimeClient.from_port_file(port, timeout=5.0)
                    client.request("runtime.describe", timeout=1.0)
                except Exception:
                    client = None
            time.sleep(0.1)
        if client is None:
            entry["error"] = "runtime did not come up"
        else:
            text = ""
            started = time.monotonic()
            first_text_ms = None
            while time.monotonic() - started < 15:
                try:
                    snapshot = client.request("runtime.snapshot", timeout=5.0)["result"]
                    window = snapshot["windows"][0]
                    pane = window["tabs"][0]["panes"][0]
                    read = client.request(
                        "pane.read", {"window_id": window["id"], "pane_id": pane["id"], "lines": 40}, timeout=5.0
                    )["result"]
                    text = read.get("text", "")
                    entry["exited"] = read.get("exited")
                    entry["cwd"] = pane.get("cwd")
                    if text.strip() and first_text_ms is None:
                        first_text_ms = round((time.monotonic() - started) * 1000)
                    if MARKER in text:
                        break
                    if env_dump is not None and env_dump.is_file() and env_dump.stat().st_size > 0:
                        time.sleep(1.0)
                        break
                except Exception as error:
                    entry["read_error"] = str(error)
                time.sleep(0.25)
            entry["first_text_ms"] = first_text_ms
            entry["marker"] = MARKER in text
            entry["text"] = text.strip()[:600]
        if env_dump is not None and env_dump.is_file():
            parsed: dict[str, str] = {}
            for line in env_dump.read_text(encoding="mbcs", errors="replace").splitlines():
                name, separator, value = line.partition("=")
                if separator and name and not name.startswith("="):
                    parsed[name] = value
            if parsed:
                # Windows names are case-insensitive; os.environ upper-cases them.
                app_env = {name.upper(): value for name, value in parsed.items()}
                entry["app_env_count"] = len(parsed)
        # The shell keeps the work directory as its cwd; take the whole tree down.
        subprocess.run(["taskkill", "/T", "/F", "/PID", str(process.pid)], capture_output=True)
        process.wait(10)
        log.close()
        entry["app_log"] = [
            line for line in (Path(root) / "app.log").read_text(encoding="utf-8", errors="replace").splitlines()
            if "prepaint" not in line and "resize-trace" not in line
        ][:40]
        print(name, json.dumps({k: v for k, v in entry.items() if k != "app_log"}, ensure_ascii=False)[:400], flush=True)
        return entry


def replay(name: str, env: dict[str, str] | None, cwd: str | None) -> bool:
    """Run PowerShell 5.1 through the smoke path; True when the marker rendered."""
    library = os.fspath(runtime_library) if runtime_library.is_file() else None
    command = subprocess.list2cmdline(POWERSHELL_51)
    result = conpty_smoke.run_case(library, command, 0, REPLAY_WAIT, done_marker=MARKER.encode(), env=env, cwd=cwd)
    passed = result.get("marker_ms") is not None
    summary = {k: result.get(k) for k in ("error", "first_output_ms", "marker_ms", "output_bytes")}
    print("replay", name, "PASS" if passed else "SILENT", json.dumps(summary), flush=True)
    report.setdefault("replays", {})[name] = summary | {"passed": passed}  # type: ignore[index]
    return passed


def bisect(base: dict[str, str], target: dict[str, str]) -> None:
    """Find which of the variables that differ between base and target break PS 5.1."""
    differing = sorted({*base, *target}, key=str.upper)
    differing = [name for name in differing if base.get(name) != target.get(name)]
    report["env_diff"] = {name: {"runner": base.get(name), "app": target.get(name)} for name in differing}
    print(f"bisect over {len(differing)} differing variables", flush=True)

    def with_changes(names: list[str]) -> dict[str, str]:
        env = dict(base)
        for name in names:
            if name in target:
                env[name] = target[name]
            else:
                env.pop(name, None)
        return env

    if replay("runner-env-plus-all-diffs", with_changes(differing), None):
        report["bisect"] = "applying every differing variable to the runner environment still passes"
        return
    suspects = list(differing)
    rounds = 0
    while len(suspects) > 1 and rounds < 12:
        rounds += 1
        half = suspects[: len(suspects) // 2]
        if not replay(f"round{rounds}-first-half({len(half)})", with_changes(half), None):
            suspects = half
            continue
        rest = suspects[len(suspects) // 2 :]
        if not replay(f"round{rounds}-second-half({len(rest)})", with_changes(rest), None):
            suspects = rest
            continue
        report["bisect"] = f"no single half reproduces; remaining suspects: {suspects}"
        return
    report["bisect"] = {"culprits": suspects, "values": {name: target.get(name) for name in suspects}}
    print("bisect result:", report["bisect"], flush=True)


def redacted(env: dict[str, str] | None) -> dict[str, str] | None:
    if env is None:
        return None
    secret = ("TOKEN", "SECRET", "PASSWORD", "_KEY")
    return {name: ("<redacted>" if any(part in name.upper() for part in secret) else value) for name, value in env.items()}


# Same shape as the panes' working directory (under TEMP), but it outlives boot().
app_cwd = os.fspath(Path(tempfile.mkdtemp(prefix="pebrel-cmd-")) / "work")
os.mkdir(app_cwd)
report["cmd-env"] = boot("cmd-env", ["cmd.exe", "/d", "/k"], env_dump=Path("app-env.txt"))
report["powershell"] = boot("powershell", POWERSHELL_51)
report["runner_env"] = redacted(dict(os.environ))
report["app_env"] = redacted(app_env)
if app_env is not None:
    control = replay("inherited-env-inherited-cwd", None, None)
    env_only = replay("app-env-inherited-cwd", app_env, None)
    cwd_only = replay("inherited-env-app-cwd", None, app_cwd)
    replay("app-env-app-cwd", app_env, app_cwd)
    if control and not env_only:
        bisect(dict(os.environ), app_env)
    if isinstance(report.get("bisect"), dict) and report["bisect"]["culprits"] == ["PSMODULEPATH"]:  # type: ignore[index]
        # Which entry of the registry-only module path stalls PowerShell 5.1,
        # and is the stall tied to the interactive host (PSReadLine import)?
        runner_entries = [entry for entry in os.environ.get("PSMODULEPATH", "").split(";") if entry]
        app_entries = [entry for entry in app_env.get("PSMODULEPATH", "").split(";") if entry]
        windows_default = [
            entry for entry in app_entries
            if entry.lower().startswith((r"c:\program files\windowspowershell\modules", r"c:\windows\system32"))
        ]
        pwsh_entries = [entry for entry in runner_entries if entry not in app_entries]
        variants: dict[str, tuple[list[str], list[str]]] = {
            "app-entries": (app_entries, POWERSHELL_51),
            "pwsh-dirs-then-app-entries": (pwsh_entries + app_entries, POWERSHELL_51),
            "windows-default-only": (windows_default, POWERSHELL_51),
            "windows-default-non-interactive": (
                windows_default,
                ["powershell.exe", "-NoLogo", "-NoProfile", "-Command", f"Write-Output {MARKER}"],
            ),
            "windows-default-import-psreadline-non-interactive": (
                windows_default,
                ["powershell.exe", "-NoLogo", "-NoProfile", "-Command", f"Import-Module PSReadLine; Write-Output {MARKER}"],
            ),
            "windows-default-list-psreadline-non-interactive": (
                windows_default,
                ["powershell.exe", "-NoLogo", "-NoProfile", "-Command",
                 f"Get-Module -ListAvailable PSReadLine | ForEach-Object {{ $_.Version.ToString() + ' ' + $_.Path }}; Write-Output {MARKER}"],
            ),
        }
        for index, entry in enumerate(app_entries):
            variants[f"app-entries-without-{index}"] = ([e for e in app_entries if e != entry], POWERSHELL_51)
        for name, (entries, command) in variants.items():
            env = dict(os.environ)
            env["PSMODULEPATH"] = ";".join(entries)
            library = os.fspath(runtime_library) if runtime_library.is_file() else None
            result = conpty_smoke.run_case(
                library, subprocess.list2cmdline(command), 0, REPLAY_WAIT, done_marker=MARKER.encode(), env=env, cwd=None
            )
            summary = {k: result.get(k) for k in ("error", "first_output_ms", "marker_ms", "output_bytes")}
            summary["output_head"] = str(result.get("output_head", ""))[:300]
            summary["entries"] = entries
            print("psmodulepath", name, "PASS" if result.get("marker_ms") is not None else "SILENT", json.dumps(summary)[:400], flush=True)
            report.setdefault("psmodulepath_variants", {})[name] = summary  # type: ignore[index]
    if isinstance(report.get("env_diff"), dict):
        report["env_diff"] = {
            name: {side: redacted({name: value})[name] if value is not None else None for side, value in sides.items()}  # type: ignore[index]
            for name, sides in report["env_diff"].items()  # type: ignore[union-attr]
        }
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
