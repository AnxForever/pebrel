"""Boot Pebrel with explicit `-e` commands and dump what each pane renders.

Separates "the shell hangs at startup" from "the PTY read path is dead":
cmd, PowerShell 5.1 and pwsh are launched through the same ConPTY and
environment path as the default tab, and every pane is read for 15s.
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
from conformance.harness import RuntimeClient  # noqa: E402

app = Path(sys.argv[1]).resolve()
output = Path(sys.argv[2])
commands = {
    "cmd": ["cmd.exe", "/d", "/k", "echo PEBREL_SMOKE_OK"],
    "powershell": ["powershell.exe", "-NoLogo", "-NoProfile", "-NoExit", "-Command", "Write-Output PEBREL_SMOKE_OK"],
    "powershell-profile": ["powershell.exe", "-NoLogo", "-NoExit", "-Command", "Write-Output PEBREL_SMOKE_OK"],
    "pwsh": ["pwsh.exe", "-NoLogo", "-NoProfile", "-NoExit", "-Command", "Write-Output PEBREL_SMOKE_OK"],
}
report: dict[str, object] = {}
for name, command in commands.items():
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
                    if "PEBREL_SMOKE_OK" in text:
                        break
                except Exception as error:
                    entry["read_error"] = str(error)
                time.sleep(0.25)
            entry["first_text_ms"] = first_text_ms
            entry["marker"] = "PEBREL_SMOKE_OK" in text
            entry["text"] = text.strip()[:600]
        # The shell keeps the work directory as its cwd; take the whole tree down.
        subprocess.run(["taskkill", "/T", "/F", "/PID", str(process.pid)], capture_output=True)
        process.wait(10)
        log.close()
        entry["app_log"] = [
            line for line in (Path(root) / "app.log").read_text(encoding="utf-8", errors="replace").splitlines()
            if "prepaint" not in line and "resize-trace" not in line
        ][:40]
        report[name] = entry
        print(name, json.dumps({k: v for k, v in entry.items() if k != "app_log"}, ensure_ascii=False)[:400], flush=True)
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
