"""Exercise the demo in a real PTY and check terminal-mode restoration (Unix)."""
import fcntl
import json
import shlex
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def main():
    binary = str(Path(sys.argv[1]).resolve())
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 110, 0, 0))
    before = termios.tcgetattr(slave)

    def session():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    with tempfile.TemporaryDirectory(prefix="aitop-pty-") as home:
        manual = "--manual-refresh" in sys.argv[2:]
        if manual:
            root=Path(home)
            (root/".config/aitop").mkdir(parents=True)
            (root/".config/aitop/config.json").write_text(json.dumps({"show_agy":True,"refresh_interval_secs":300}))
            (root/".local/bin").mkdir(parents=True)
            fixture=(Path(__file__).resolve().parents[1]/"tests/fixtures/agy-quota.json").read_text()
            counter=shlex.quote(str(root/"query-count"))
            script=f"""#!/bin/sh
if [ \"$1\" = '--version' ]; then echo 1.5.3; exit 0; fi
n=0
if [ -f {counter} ]; then n=$(cat {counter}); fi
printf '%s\\n' \"$((n+1))\" > {counter}
cat <<'QUOTA'
{fixture}
QUOTA
"""
            (root/".local/bin/agy").write_text(script)
            (root/".local/bin/agy").chmod(0o700)
        color_check = "--color-check" in sys.argv[2:]
        launch='"$1" --home "$2"'+(' --color' if color_check else '')+('' if manual else ' demo')
        shell=launch+'; code=$?; printf "\\nAITOP_EXIT:%s\\n" "$code"; IFS= read -r line; exit "$code"'
        child = subprocess.Popen(["/bin/sh","-c",shell,"aitop-pty",binary,home], stdin=slave, stdout=slave, stderr=slave, preexec_fn=session)
        output = bytearray()

        def expect(text):
            deadline = time.monotonic() + 4
            while time.monotonic() < deadline:
                plain=re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]",b"",bytes(output)).replace(b" ",b"")
                if text.replace(b" ",b"") in plain:
                    return
                if select.select([master], [], [], 0.1)[0]:
                    output.extend(os.read(master, 65536))
            raise AssertionError(f"Terminal did not display {text!r}; exit={child.poll()}; output={bytes(output[-500:])!r}")

        try:
            if manual:
                expect(b"Local monitor")
                assert (Path(home)/"query-count").read_text().strip()=="1"
                os.write(master,b"r")
                deadline=time.monotonic()+4
                while time.monotonic()<deadline:
                    if select.select([master],[],[],0.05)[0]: output.extend(os.read(master,65536))
                    if (Path(home)/"query-count").read_text().strip()=="2": break
                assert (Path(home)/"query-count").read_text().strip()=="2", "r reused the 60-second cache"
            else:
                expect(b"DEMO DATA")
            if manual:
                pass
            elif "--details" in sys.argv[2:]:
                os.write(master, b"\r")
                expect(b"ACCOUNT LIMITS")
                os.write(master, b"\x1b[6~")
                expect(b"Peak")
                os.write(master, b"c")
                expect(b"(API estimate)")
            else:
                os.write(master, b"2")
                expect(b"workspace-api")
                if color_check:
                    assert b"38;5;42" in output, "--color did not restore the calendar's color output"
                os.write(master, b"g")
                expect(b"Model")
                # A resize exercises the compact layout and forces a complete frame;
                # normal Ratatui updates contain only changed cells, not full words.
                fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",24,80,0,0))
                os.kill(child.pid,signal.SIGWINCH)
                expect(b"demo-model-a")
                os.write(master, b"3")
                expect(b"SETTINGS / CONNECTIONS")
                expect(b"Refresh interval     5m")
                # Force complete frames; changed digits otherwise arrive as
                # incremental terminal updates rather than complete labels.
                width = 80

                def change_and_redraw(key, text):
                    nonlocal width
                    output.clear()
                    os.write(master, key)
                    # Let the key reach the event loop before requesting a full
                    # frame; a resize can otherwise render the previous value.
                    deadline = time.monotonic() + 0.15
                    while time.monotonic() < deadline:
                        if select.select([master], [], [], 0.02)[0]:
                            os.read(master, 65536)
                    output.clear()
                    width = 81 if width == 80 else 80
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, width, 0, 0))
                    os.kill(child.pid, signal.SIGWINCH)
                    expect(text)

                for label in (b"3m", b"1m", b"30s", b"30s"):
                    change_and_redraw(b"h", b"Refresh interval     " + label)
                for label in (b"1m", b"3m", b"5m", b"10m", b"15m", b"15m"):
                    change_and_redraw(b"l", b"Refresh interval     " + label)
                change_and_redraw(b"2", b"demo-model-a")
                change_and_redraw(b"3", b"SETTINGS / CONNECTIONS")
                change_and_redraw(b"1", b"Updated")
                os.write(master, b"\r")
                expect(b"ACCOUNT LIMITS")
            os.write(master, b"\x03" if "--ctrl-c" in sys.argv[2:] else b"q")
            expect(b"AITOP_EXIT:0")
            after = termios.tcgetattr(slave)
            mask = termios.ICANON | termios.ECHO
            assert before[3] & mask == after[3] & mask, "Terminal was left in raw mode"
            os.write(master,b"\n")
            deadline=time.monotonic()+4
            while child.poll() is None and time.monotonic()<deadline:
                # Keep draining output while the app flushes/restores its terminal.
                if select.select([master],[],[],0.05)[0]:
                    try: output.extend(os.read(master,65536))
                    except OSError: break
            assert child.wait(timeout=1) == 0
            if not manual:
                assert not list(Path(home).iterdir()), "Demo wrote user files"
            if manual:
                print("PTY: r invoked the provider again immediately within the automatic cache window; terminal restored")
            elif "--details" in sys.argv[2:]:
                print("PTY: details, scrolling, USD chart, quit and terminal restoration passed")
            else:
                print("PTY: view switching from Settings, six refresh presets, grouping, resize, details, quit and terminal restoration passed")
        finally:
            if child.poll() is None:
                child.kill()
            os.close(master)
            os.close(slave)
            child.wait(timeout=2)


if __name__ == "__main__":
    main()
