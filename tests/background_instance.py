#!/usr/bin/env python3
"""Headless CLI lifecycle regression, with its own D-Bus and mock identity.

Run: python3 tests/background_instance.py [target/debug/omarchygram]
No Telegram credentials, real notifications or desktop windows are used.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def inside(binary):
    assert os.environ.get('WAYLAND_DISPLAY', '').startswith('wayland-omg-'), 'bin/headless is required'
    env = dict(os.environ, OMG_SMOKE_SINGLE_INSTANCE='1')
    with tempfile.TemporaryFile(mode='w+') as log:
        primary = subprocess.Popen([binary, '--smoke', '--background'], env=env, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                owner = subprocess.run([
                    'gdbus', 'call', '--session', '--dest', 'org.freedesktop.DBus',
                    '--object-path', '/org/freedesktop/DBus', '--method',
                    'org.freedesktop.DBus.NameHasOwner', 'dev.leoom.Omarchygram.Smoke',
                ], capture_output=True, text=True, check=True)
                if 'true' in owner.stdout:
                    break
                assert primary.poll() is None, 'background startup exited'
                time.sleep(0.05)
            else:
                raise AssertionError('background instance never registered')
            time.sleep(2)
            assert primary.poll() is None, 'hidden application must stay running'
            subprocess.run([binary, '--smoke'], env=env, stdout=log, stderr=log, check=True, timeout=8)
            assert primary.poll() is None, 'reopening must preserve original process'
            time.sleep(0.2)
            subprocess.run([binary, '--smoke', '--quit'], env=env, stdout=log, stderr=log, check=True, timeout=8)
            assert primary.wait(timeout=8) == 0, 'quit must shut down the original process'
            print('PASS: background startup, single-instance reopen, explicit quit')
        except BaseException:
            log.seek(0)
            print(log.read(), file=sys.stderr)
            raise
        finally:
            if primary.poll() is None:
                primary.terminate()
                primary.wait(timeout=8)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == '--inside':
        inside(sys.argv[2])
        return
    binary = str((ROOT / (sys.argv[1] if len(sys.argv) > 1 else 'target/debug/omarchygram')).resolve())
    # No desktop service auto-activation: an isolated accessibility launcher
    # otherwise competes for the user's at-spi socket. The full native gate
    # retains normal accessibility; this test only exercises D-Bus lifecycle.
    with tempfile.TemporaryDirectory(prefix='omg-instance-') as tmp:
        config = Path(tmp) / 'bus.conf'
        config.write_text('''<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>''')
        env = dict(os.environ, G_DEBUG='fatal-criticals', GTK_A11Y='none', GIO_USE_VFS='local')
        subprocess.run([
            str(ROOT / 'bin/headless'), 'dbus-run-session', f'--config-file={config}',
            sys.executable, str(Path(__file__).resolve()), '--inside', binary,
        ], cwd=ROOT, env=env, check=True, timeout=45)


if __name__ == '__main__':
    main()
