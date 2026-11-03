#!/usr/bin/env python3
"""One real xHCI keyboard/editor/save/reboot check; serial is output-only."""
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import socket
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parents[1]
subprocess.run([str(root / 'run-miraos.sh'), '--build'], cwd=root, check=True, timeout=120)
out = root / 'target/miraos/smoke'
out.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='mira-usb-keyboard-') as directory:
    tmp = Path(directory)
    firmware = Path(os.environ.get('MIRA_OVMF_DIR', '/usr/share/edk2/x64'))
    shutil.copyfile(firmware / 'OVMF_VARS.4m.fd', tmp / 'vars.fd')
    with (tmp / 'state.img').open('xb') as state:
        state.truncate(2 * 1024 * 1024)
        state.write(b'MIRAVOL2' + (2).to_bytes(8, 'little'))
    command = ['qemu-system-x86_64', '-accel', 'tcg', '-machine', 'q35', '-m', '128M',
        '-vga', 'std', '-display', 'none', '-nic', 'none', '-monitor', 'none',
        '-drive', f'if=pflash,format=raw,readonly=on,file={firmware}/OVMF_CODE.4m.fd',
        '-drive', f'if=pflash,format=raw,file={tmp}/vars.fd',
        '-drive', f'if=none,id=esp,format=raw,readonly=on,file=fat:{root}/target/miraos/esp',
        '-device', 'virtio-blk-pci,drive=esp,bootindex=1',
        '-device', 'qemu-xhci,id=xhci,addr=04.0',
        '-device', 'usb-kbd,bus=xhci.0,port=1',
        '-blockdev', json.dumps({'driver': 'raw', 'node-name': 'mira-state',
            'file': {'driver': 'file', 'filename': str(tmp / 'state.img')}}),
        '-device', 'virtio-blk-pci,drive=mira-state,addr=06.0,disable-modern=on,vectors=0,queue-size=256,serial=MIRA-EXPERIENCE',
        '-serial', 'stdio', '-no-reboot', '-no-shutdown',
        '-qmp', f'unix:{tmp}/qmp.sock,server=on,wait=off']
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    transcript = bytearray()
    sock = None
    stream = None
    deadline = time.monotonic() + 120

    def pump(seconds=.05):
        if time.monotonic() > deadline:
            raise RuntimeError('USB keyboard test exceeded 120 seconds')
        for key, _ in selector.select(seconds):
            transcript.extend(key.fileobj.read1(8192))

    def wait(marker, start=0, seconds=30):
        end = min(deadline, time.monotonic() + seconds)
        while marker not in transcript[start:]:
            if process.poll() is not None or time.monotonic() > end:
                raise RuntimeError(f'Missing serial marker {marker!r}')
            pump()

    def settle(seconds=.12):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            pump(min(.02, end - time.monotonic()))

    def qmp(name, arguments=None):
        stream.write((json.dumps({'execute': name, 'arguments': arguments or {}}) + '\n').encode())
        while True:
            message = json.loads(stream.readline())
            if 'error' in message:
                raise RuntimeError(message['error'])
            if 'return' in message:
                return message['return']

    def press(*keys):
        # QEMU QMP reference: https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html#command-input-send-event
        for down, ordered in ((True, keys), (False, reversed(keys))):
            qmp('input-send-event', {'events': [
                {'type': 'key', 'data': {'down': down, 'key': {'type': 'qcode', 'data': key}}}
                for key in ordered]})
            settle()

    def console_command(letter, marker):
        start = len(transcript)
        press('shift', 'semicolon')
        press(letter)
        press('ret')
        wait(marker, start, 8)
        return start

    def report(start):
        wait(b'ready> ', start, 8)
        found = re.search(rb'source bytes/cursor/revision/dirty/full/rejected: (\d+)/(\d+)/(\d+)/(\d+)/(\d+)/(\d+)\r?\nsource line: ([^\r\n]*)', transcript[start:])
        if not found:
            raise AssertionError('Source report was missing or malformed')
        return tuple(map(int, found.groups()[:6])), found.group(7)

    def connect_keyboard(start=0):
        global sock, stream
        wait(b'ASCII/US boot keyboard;', start)
        wait(b'ready> ', start)
        assert re.search(rb'USB keyboard ready/error/port/VID:PID: 1/0/[1-9]\d*/[0-9A-Fa-f]{4}:[0-9A-Fa-f]{4}', transcript[start:]), 'USB boot keyboard did not enumerate successfully'
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(5)
        sock.connect(str(tmp / 'qmp.sock'))
        stream = sock.makefile('rwb', buffering=0)
        assert 'QMP' in json.loads(stream.readline())
        qmp('qmp_capabilities')

    def stop_vm():
        global sock, stream
        console_command('q', b'halted')
        qmp('quit')
        process.wait(timeout=5)
        selector.unregister(process.stdout)
        process.stdout.close()
        stream.close()
        sock.close()
        stream = sock = None

    try:
        connect_keyboard()
        initial, _ = report(console_command('d', b'source bytes/cursor'))
        assert initial[1:] == (0, 0, 0, 0, 0), initial
        console_command('b', b'Source draft:')
        press('home')
        for keys in [('shift', 'u'), ('shift', 's'), ('shift', 'b'), ('spc',),
                     ('shift', 'k'), ('e',), ('y',), ('ret',), ('up',), ('right',),
                     ('shift', 'z'), ('ctrl', 'z'), ('right',), ('backspace',),
                     ('left',), ('shift', 'k'), ('right',)]:
            press(*keys)
        start = len(transcript)
        press('ctrl', 'x')
        wait(b'source bytes/cursor', start, 8)
        final, line = report(start)
        assert line == b'KUB Key', (line, final)
        assert final == (initial[0] + 8, 2, 12, 1, 0, 0), (initial, final)
        # A fresh USB console command confirms Ctrl+X actually changed input mode.
        after, after_line = report(console_command('d', b'source bytes/cursor'))
        assert (after, after_line) == (final, line), (after, after_line)
        console_command('b', b'Source draft:')
        start = len(transcript)
        press('ctrl', 's')
        wait(b'source draft saved;', start, 12)
        start = len(transcript)
        press('ctrl', 'x')
        wait(b'source bytes/cursor', start, 8)
        saved, saved_line = report(start)
        assert saved == (final[0], 2, 12, 0, 0, 0) and saved_line == line, (saved, saved_line)
        assert b'source storage generation/restored/blocked/different-base: 1/0/0/0' in transcript[start:]
        stop_vm()
        start = len(transcript)
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        selector.register(process.stdout, selectors.EVENT_READ)
        connect_keyboard(start)
        start = console_command('d', b'source bytes/cursor')
        restored, restored_line = report(start)
        assert (restored, restored_line) == (saved, saved_line), (restored, restored_line)
        assert b'source storage generation/restored/blocked/different-base: 1/1/0/0' in transcript[start:]
        stop_vm()
        print('PASS: xHCI USB keyboard, Shift+colon/:b, text, Shift, Ctrl+Z, Backspace, arrows, Ctrl+X, Ctrl+S and source retained after reboot')
    except Exception as error:
        raise RuntimeError(str(error) + '\n' + transcript.decode(errors='replace')[-3500:]) from error
    finally:
        (out / 'usb-keyboard.log').write_bytes(transcript)
        if stream is not None:
            stream.close()
        if sock is not None:
            sock.close()
        selector.close()
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
