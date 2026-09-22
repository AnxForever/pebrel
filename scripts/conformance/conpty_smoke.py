"""Spawn a shell through ConPTY without Pebrel and report its first output.

Separates runtime failures (conpty.dll, OpenConsole.exe, the shell itself)
from application failures when a platform renders no initial shell output.
Both the side-loaded pair and the in-box kernel32 ConPTY are exercised.
"""

from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes as wt
import json
import os
import shutil
import sys
import threading
import time

PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = 0x00020016
EXTENDED_STARTUPINFO_PRESENT = 0x00080000
STARTF_USESTDHANDLES = 0x00000100
PSEUDOCONSOLE_WIN32_INPUT_MODE = 0x4


class Coord(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class StartupInfo(ctypes.Structure):
    _fields_ = [
        ("cb", wt.DWORD), ("lpReserved", wt.LPWSTR), ("lpDesktop", wt.LPWSTR),
        ("lpTitle", wt.LPWSTR), ("dwX", wt.DWORD), ("dwY", wt.DWORD),
        ("dwXSize", wt.DWORD), ("dwYSize", wt.DWORD), ("dwXCountChars", wt.DWORD),
        ("dwYCountChars", wt.DWORD), ("dwFillAttribute", wt.DWORD),
        ("dwFlags", wt.DWORD), ("wShowWindow", wt.WORD), ("cbReserved2", wt.WORD),
        ("lpReserved2", ctypes.POINTER(wt.BYTE)), ("hStdInput", wt.HANDLE),
        ("hStdOutput", wt.HANDLE), ("hStdError", wt.HANDLE),
    ]


class StartupInfoEx(ctypes.Structure):
    _fields_ = [("StartupInfo", StartupInfo), ("lpAttributeList", wt.LPVOID)]


class ProcessInfo(ctypes.Structure):
    _fields_ = [("hProcess", wt.HANDLE), ("hThread", wt.HANDLE),
                ("dwProcessId", wt.DWORD), ("dwThreadId", wt.DWORD)]


kernel = ctypes.WinDLL("kernel32", use_last_error=True)
kernel.CreatePipe.argtypes = [ctypes.POINTER(wt.HANDLE), ctypes.POINTER(wt.HANDLE), wt.LPVOID, wt.DWORD]
kernel.CreatePipe.restype = wt.BOOL
kernel.ReadFile.argtypes = [wt.HANDLE, wt.LPVOID, wt.DWORD, ctypes.POINTER(wt.DWORD), wt.LPVOID]
kernel.ReadFile.restype = wt.BOOL
kernel.WriteFile.argtypes = [wt.HANDLE, wt.LPCVOID, wt.DWORD, ctypes.POINTER(wt.DWORD), wt.LPVOID]
kernel.WriteFile.restype = wt.BOOL
kernel.CloseHandle.argtypes = [wt.HANDLE]
kernel.CloseHandle.restype = wt.BOOL
kernel.InitializeProcThreadAttributeList.argtypes = [wt.LPVOID, wt.DWORD, wt.DWORD, ctypes.POINTER(ctypes.c_size_t)]
kernel.InitializeProcThreadAttributeList.restype = wt.BOOL
kernel.UpdateProcThreadAttribute.argtypes = [wt.LPVOID, wt.DWORD, ctypes.c_size_t, wt.LPVOID, ctypes.c_size_t, wt.LPVOID, wt.LPVOID]
kernel.UpdateProcThreadAttribute.restype = wt.BOOL
kernel.DeleteProcThreadAttributeList.argtypes = [wt.LPVOID]
kernel.CreateProcessW.argtypes = [wt.LPCWSTR, wt.LPWSTR, wt.LPVOID, wt.LPVOID, wt.BOOL, wt.DWORD, wt.LPVOID, wt.LPCWSTR, wt.LPVOID, ctypes.POINTER(ProcessInfo)]
kernel.CreateProcessW.restype = wt.BOOL
kernel.WaitForSingleObject.argtypes = [wt.HANDLE, wt.DWORD]
kernel.WaitForSingleObject.restype = wt.DWORD
kernel.GetExitCodeProcess.argtypes = [wt.HANDLE, ctypes.POINTER(wt.DWORD)]
kernel.GetExitCodeProcess.restype = wt.BOOL
kernel.TerminateProcess.argtypes = [wt.HANDLE, wt.UINT]
kernel.TerminateProcess.restype = wt.BOOL
kernel.FreeConsole.argtypes = []
kernel.FreeConsole.restype = wt.BOOL
kernel.AttachConsole.argtypes = [wt.DWORD]
kernel.AttachConsole.restype = wt.BOOL
kernel.CreateFileW.argtypes = [wt.LPCWSTR, wt.DWORD, wt.DWORD, wt.LPVOID, wt.DWORD, wt.DWORD, wt.HANDLE]
kernel.CreateFileW.restype = wt.HANDLE
kernel.GetConsoleScreenBufferInfo.argtypes = [wt.HANDLE, wt.LPVOID]
kernel.GetConsoleScreenBufferInfo.restype = wt.BOOL
INVALID_HANDLE_VALUE = wt.HANDLE(-1).value


def cursor_probe(pid: int) -> dict[str, object]:
    """The same conhost cursor probe Pebrel runs after every resize."""
    kernel.FreeConsole()
    if not kernel.AttachConsole(pid):
        return {"attach_error": ctypes.get_last_error()}
    handle = kernel.CreateFileW("CONOUT$", 0xC0000000, 0x3, None, 3, 0, None)
    if handle == INVALID_HANDLE_VALUE:
        error = ctypes.get_last_error()
        kernel.FreeConsole()
        return {"conout_error": error}
    info = ctypes.create_string_buffer(22)
    ok = kernel.GetConsoleScreenBufferInfo(handle, info)
    error = ctypes.get_last_error()
    kernel.CloseHandle(handle)
    kernel.FreeConsole()
    if not ok:
        return {"info_error": error}
    # CONSOLE_SCREEN_BUFFER_INFO: dwSize, dwCursorPosition, wAttributes, srWindow.
    cursor_y = int.from_bytes(info.raw[6:8], "little", signed=True)
    top = int.from_bytes(info.raw[12:14], "little", signed=True)
    bottom = int.from_bytes(info.raw[16:18], "little", signed=True)
    return {"row": cursor_y - top, "rows": bottom - top + 1}

CreateFn = ctypes.WINFUNCTYPE(ctypes.c_long, Coord, wt.HANDLE, wt.HANDLE, wt.DWORD, ctypes.POINTER(wt.LPVOID))
ResizeFn = ctypes.WINFUNCTYPE(ctypes.c_long, wt.LPVOID, Coord)
CloseFn = ctypes.WINFUNCTYPE(None, wt.LPVOID)


def api(library: str | None) -> tuple[CreateFn, ResizeFn, CloseFn]:
    module = ctypes.WinDLL(library, use_last_error=True) if library else kernel
    create = CreateFn(("CreatePseudoConsole", module))
    resize = ResizeFn(("ResizePseudoConsole", module))
    close = CloseFn(("ClosePseudoConsole", module))
    return create, resize, close


def run_case(
    library: str | None,
    command: str,
    flags: int,
    wait_seconds: float,
    done_marker: bytes = b"CONPTY_SMOKE_OK",
    steps: tuple[str, ...] = (),
) -> dict[str, object]:
    """Interactive shells never exit; the marker ends the wait once it renders.

    `steps` replays Pebrel's post-spawn sequence 30ms after the first output:
    `resize` calls ResizePseudoConsole, `probe` attaches to the client console
    and reads its cursor, mirroring the align-sync and align stages.
    """
    result: dict[str, object] = {
        "library": library or "kernel32", "command": command, "flags": flags, "steps": list(steps),
    }
    try:
        create, resize, close = api(library)
    except OSError as error:
        result["error"] = f"load failed: {error}"
        return result
    conout_read, conout_write = wt.HANDLE(), wt.HANDLE()
    conin_read, conin_write = wt.HANDLE(), wt.HANDLE()
    if not kernel.CreatePipe(ctypes.byref(conout_read), ctypes.byref(conout_write), None, 0) or \
       not kernel.CreatePipe(ctypes.byref(conin_read), ctypes.byref(conin_write), None, 0):
        result["error"] = f"CreatePipe failed: {ctypes.get_last_error()}"
        return result
    hpc = wt.LPVOID()
    started = time.monotonic()
    hresult = create(Coord(80, 25), conin_read, conout_write, flags, ctypes.byref(hpc))
    result["create_hresult"] = f"{hresult & 0xFFFFFFFF:#010x}"
    result["create_ms"] = round((time.monotonic() - started) * 1000)
    if hresult != 0:
        return result
    # The side-loaded host answers the same primed DA1 reply Pebrel writes.
    written = wt.DWORD()
    kernel.WriteFile(conin_write, b"\x1b[?61c", 6, ctypes.byref(written), None)

    size = ctypes.c_size_t(0)
    kernel.InitializeProcThreadAttributeList(None, 1, 0, ctypes.byref(size))
    attributes = ctypes.create_string_buffer(size.value)
    if not kernel.InitializeProcThreadAttributeList(attributes, 1, 0, ctypes.byref(size)):
        result["error"] = f"InitializeProcThreadAttributeList failed: {ctypes.get_last_error()}"
        return result
    if not kernel.UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                                            hpc, ctypes.sizeof(wt.LPVOID), None, None):
        result["error"] = f"UpdateProcThreadAttribute failed: {ctypes.get_last_error()}"
        return result
    startup = StartupInfoEx()
    startup.StartupInfo.cb = ctypes.sizeof(startup)
    # Null standard handles with USESTDHANDLES: the client binds to the
    # pseudoconsole instead of inheriting this process's console or pipes.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES
    startup.lpAttributeList = ctypes.cast(attributes, wt.LPVOID)
    process = ProcessInfo()
    cmdline = ctypes.create_unicode_buffer(command)
    if not kernel.CreateProcessW(None, cmdline, None, None, False, EXTENDED_STARTUPINFO_PRESENT,
                                 None, None, ctypes.byref(startup), ctypes.byref(process)):
        result["error"] = f"CreateProcessW failed: {ctypes.get_last_error()}"
        return result
    result["pid"] = process.dwProcessId

    chunks: list[bytes] = []

    def reader() -> None:
        buffer = ctypes.create_string_buffer(4096)
        read = wt.DWORD()
        while kernel.ReadFile(conout_read, buffer, 4096, ctypes.byref(read), None) and read.value:
            chunks.append(buffer.raw[: read.value])

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    deadline = time.monotonic() + wait_seconds
    first_output_ms: int | None = None
    marker_ms: int | None = None
    steps_at: float | None = None
    step_log: list[dict[str, object]] = []
    while time.monotonic() < deadline:
        if chunks and first_output_ms is None:
            first_output_ms = round((time.monotonic() - started) * 1000)
            steps_at = time.monotonic() + 0.03
        if steps and steps_at is not None and time.monotonic() >= steps_at:
            for step in steps:
                entry: dict[str, object] = {"step": step, "at_ms": round((time.monotonic() - started) * 1000)}
                if step == "resize":
                    entry["hresult"] = f"{resize(hpc, Coord(82, 25)) & 0xFFFFFFFF:#010x}"
                elif step == "probe":
                    entry.update(cursor_probe(process.dwProcessId))
                entry["bytes_after"] = sum(len(chunk) for chunk in chunks)
                step_log.append(entry)
            steps = ()
        if marker_ms is None and done_marker in b"".join(chunks):
            marker_ms = round((time.monotonic() - started) * 1000)
            break
        if kernel.WaitForSingleObject(process.hProcess, 100) == 0:
            break
    result["step_log"] = step_log
    result["marker_ms"] = marker_ms
    code = wt.DWORD()
    kernel.GetExitCodeProcess(process.hProcess, ctypes.byref(code))
    if code.value == 259:  # STILL_ACTIVE
        kernel.TerminateProcess(process.hProcess, 1)
        result["exit"] = "terminated after timeout"
    else:
        result["exit"] = code.value
    # The in-box host flushes trailing output while ClosePseudoConsole drains
    # the pipe; the reader thread keeps consuming during that call.
    if conout_write is not None:
        kernel.CloseHandle(conout_write)
    close(hpc)
    thread.join(2.0)
    output = b"".join(chunks)
    if chunks and first_output_ms is None:
        first_output_ms = round((time.monotonic() - started) * 1000)
    result["first_output_ms"] = first_output_ms
    result["output_bytes"] = len(output)
    result["output_head"] = output[:400].decode("utf-8", "replace")
    kernel.CloseHandle(process.hThread)
    kernel.CloseHandle(process.hProcess)
    kernel.DeleteProcThreadAttributeList(attributes)
    for handle in (conin_write, conin_read, conout_read):
        if handle is not None:
            kernel.CloseHandle(handle)
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime", help="directory holding conpty.dll and OpenConsole.exe")
    parser.add_argument("--wait", type=float, default=15.0)
    parser.add_argument("--output")
    parser.add_argument(
        "--prompt-script",
        help="also start Windows PowerShell interactively with this integration script, like Pebrel does",
    )
    args = parser.parse_args()
    # Terminal bytes include private-use glyphs that legacy code pages reject.
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    shells = []
    for name in ("pwsh", "powershell", "cmd"):
        path = shutil.which(name)
        if path:
            shells.append((name, path))
    libraries: list[str | None] = [None]
    if args.runtime:
        libraries.insert(0, os.path.join(os.path.abspath(args.runtime), "conpty.dll"))

    report: dict[str, object] = {
        "architecture": os.environ.get("PROCESSOR_ARCHITECTURE"),
        "python": sys.version,
        "shells": dict(shells),
        "cases": [],
    }
    commands = {
        "pwsh": '"{path}" -NoLogo -NoProfile -Command "Write-Output CONPTY_SMOKE_OK"',
        "powershell": '"{path}" -NoLogo -NoProfile -Command "Write-Output CONPTY_SMOKE_OK"',
        "cmd": '"{path}" /d /c "echo CONPTY_SMOKE_OK"',
    }
    for library in libraries:
        for name, path in shells:
            for flags in (PSEUDOCONSOLE_WIN32_INPUT_MODE, 0):
                case = run_case(library, commands[name].format(path=path), flags, args.wait)
                case["shell"] = name
                print(json.dumps(case, ensure_ascii=False), flush=True)
                report["cases"].append(case)
        powershell = dict(shells).get("powershell")
        if args.prompt_script and powershell:
            # The prompt function emits OSC 133;A once PowerShell reaches its
            # first interactive prompt; a bare interactive shell shows "PS ".
            script = os.path.abspath(args.prompt_script)
            interactive = {
                "powershell-interactive": (f'"{powershell}" -NoLogo -NoExit', b"PS "),
                "powershell-integration": (
                    f'"{powershell}" -NoLogo -NoExit -ExecutionPolicy Bypass -Command ". \'{script}\'"',
                    b"133;A",
                ),
            }
            variants: list[tuple[str, ...]] = [
                (),
                ("resize", "probe", "probe"),
                ("early_close",),
                ("early_close", "resize", "probe", "probe"),
            ]
            for name, (command, marker) in interactive.items():
                for steps in variants:
                    case = run_case(
                        library, command, PSEUDOCONSOLE_WIN32_INPUT_MODE, args.wait, marker, steps,
                    )
                    case["shell"] = name
                    print(json.dumps(case, ensure_ascii=False), flush=True)
                    report["cases"].append(case)
    if args.output:
        os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
        with open(args.output, "w", encoding="utf-8") as handle:
            json.dump(report, handle, ensure_ascii=False, indent=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
