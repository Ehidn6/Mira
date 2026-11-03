#!/usr/bin/env python3
"""One graphical USB compact-keyboard navigation/repeat check; serial is output-only."""
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import socket
import struct
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parents[1]
subprocess.run(['cargo','run','-q','-p','mira-lang','--','build-uefi','projects/MiraOS','target/miraos/compact-navigation-test-esp'],cwd=root,check=True,timeout=120)
out = root / 'target/miraos/smoke'
out.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='mira-compact-navigation-') as directory:
    tmp = Path(directory)
    firmware = Path(os.environ.get('MIRA_OVMF_DIR', '/usr/share/edk2/x64'))
    shutil.copyfile(firmware / 'OVMF_VARS.4m.fd', tmp / 'vars.fd')
    with (tmp / 'state.img').open('xb') as state:
        state.truncate(2 * 1024 * 1024)
        state.write(b'MIRAVOL2' + (2).to_bytes(8, 'little'))
    command = ['qemu-system-x86_64', '-accel', 'tcg', '-machine', 'q35', '-m', '128M',
        '-vga', 'std', '-display', 'gtk', '-name', 'MiraOS - compact navigation check', '-nic', 'none', '-monitor', 'none',
        '-drive', f'if=pflash,format=raw,readonly=on,file={firmware}/OVMF_CODE.4m.fd',
        '-drive', f'if=pflash,format=raw,file={tmp}/vars.fd',
        '-drive', f'if=none,id=esp,format=raw,readonly=on,file=fat:{root}/target/miraos/compact-navigation-test-esp',
        '-device', 'virtio-blk-pci,drive=esp,bootindex=1',
        '-device', 'qemu-xhci,id=xhci,addr=04.0',
        '-device', 'usb-kbd,bus=xhci.0,port=1',
        '-blockdev', json.dumps({'driver': 'raw', 'node-name': 'mira-state',
            'file': {'driver': 'file', 'filename': str(tmp / 'state.img')}}),
        '-device', 'virtio-blk-pci,drive=mira-state,addr=06.0,disable-modern=on,vectors=0,queue-size=256,serial=MIRA-EXPERIENCE',
        '-debugcon', f'file:{out}/compact-navigation-debug.log', '-serial', 'stdio', '-no-reboot', '-no-shutdown',
        '-qmp', f'unix:{tmp}/qmp.sock,server=on,wait=off']
    previous_focus = None
    if os.environ.get('HYPRLAND_INSTANCE_SIGNATURE') and shutil.which('hyprctl'):
        active = subprocess.run(['hyprctl', 'activewindow', '-j'], capture_output=True,
            text=True, timeout=5)
        if active.returncode == 0:
            try:
                address = json.loads(active.stdout).get('address', '')
                if re.fullmatch(r'0x[0-9a-fA-F]+', address) and int(address, 16):
                    previous_focus = address
            except (ValueError, TypeError):
                pass
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    transcript = bytearray()
    sock = None
    stream = None
    deadline = time.monotonic() + 180

    def pump(seconds=.05):
        if time.monotonic() > deadline:
            raise RuntimeError('USB keyboard test exceeded 180 seconds')
        for key, _ in selector.select(seconds):
            transcript.extend(key.fileobj.read1(8192))

    def wait(marker, start=0, seconds=30):
        end = min(deadline, time.monotonic() + seconds)
        while marker not in transcript[start:]:
            if process.poll() is not None or time.monotonic() > end:
                raise RuntimeError(f'Missing serial marker {marker!r}')
            pump()

    def settle(seconds=.07):
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
        type_text(letter)
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

    def capture(name, seconds=8):
        path = out / (name + '.ppm')
        previous = None
        end = min(deadline, time.monotonic()+seconds)
        while time.monotonic() < end:
            settle(.2)
            qmp('screendump', {'filename': str(path)})
            with path.open('rb') as picture:
                assert picture.readline().strip() == b'P6'
                width, height = map(int, picture.readline().split())
                assert picture.readline().strip() == b'255'
                pixels = picture.read()
            # Paint is cooperative and can span host display refreshes. Ignore
            # only the live status footer when waiting for a complete static view.
            visible = pixels[:width*(height-32)*3]
            if previous == visible:
                return width, height, pixels
            previous = visible
        raise RuntimeError('Native view did not finish painting: ' + name)

    def crop(picture, x0, y0, x1, y1):
        width, _, pixels = picture
        return b''.join(pixels[(row * width + x0)*3:(row * width + x1)*3]
                        for row in range(y0, y1))

    def type_text(text):
        punctuation = {' ': ('spc',), '\n': ('ret',), '_': ('shift', 'minus'),
            '(': ('shift', '9'), ')': ('shift', '0'), ':': ('shift', 'semicolon'),
            '=': ('equal',), '.': ('dot',), '-': ('minus',), '>': ('shift', 'dot')}
        for char in text:
            if char in punctuation:
                press(*punctuation[char])
            elif char.isupper():
                press('shift', char.lower())
            else:
                press(char)

    def inspect_document():
        start = len(transcript)
        press('ctrl', 'x')
        wait(b'source bytes/cursor', start, 8)
        return report(start)

    def hold(keys, seconds):
        for down, ordered in ((True, keys), (False, reversed(keys))):
            qmp('input-send-event', {'events': [
                {'type': 'key', 'data': {'down': down, 'key': {'type': 'qcode', 'data': key}}}
                for key in ordered]})
            settle(seconds if down else .15)

    def doc_start():
        press('alt', 'shift', 'comma')

    def expect_document(content, cursor, dirty=1):
        observed, line = inspect_document()
        expected_line = content[content.rfind(b'\n', 0, cursor)+1:].split(b'\n', 1)[0]
        assert (observed[0], observed[1], observed[3:]) == (len(content), cursor, (dirty, 0, 0)), observed
        assert line == expected_line[:96], (line, expected_line[:96], observed)
        press('meta_l', '2')
        return observed

    try:
        connect_keyboard()
        # GTK remains visible, while physical host typing returns to its prior window.
        # QMP injects into the guest independently of the host window's focus.
        if previous_focus:
            subprocess.run(['hyprctl', 'dispatch',
                f'hl.dsp.focus({{window="address:{previous_focus}"}})'],
                check=True, stdout=subprocess.DEVNULL, timeout=5)
        initial, _ = report(console_command('d', b'source bytes/cursor'))
        original = (root / 'target/miraos/compact-navigation-test-esp/source.mira').read_bytes()
        assert initial == (len(original), 0, 0, 0, 0, 0), initial
        signatures = []
        for key in ('1', '2', '3'):
            press('meta_l', key)
            picture = capture('compact-navigation-layout-' + key)
            signatures.append(crop(picture, 0, 32, 160, 48))
        assert len(set(signatures)) == 3, 'Super layouts were not distinct'
        press('meta_r', '2')
        fixture = b'alpha_beta gamma delta\nrow_one\nrow_two\n'
        type_text(fixture.decode())
        content = fixture + original
        doc_start()
        expect_document(content, 0)
        # Navigation uses letter keys and punctuation, without a function-key layer.
        press('ctrl_r', 'e')
        expect_document(content, 22)
        press('ctrl_r', 'a')
        press('alt_r', 'f')
        expect_document(content, 10)
        press('alt_r', 'b')
        expect_document(content, 0)
        press('alt_r', 'shift_r', 'dot')
        expect_document(content, len(content))
        press('alt_r', 'shift_r', 'comma')
        picture = capture('compact-navigation-start')
        page_rows = picture[1]//16 - 4
        lines = content.splitlines(keepends=True)
        press('ctrl', 'v')
        expect_document(content, sum(map(len, lines[:page_rows])))
        press('alt', 'v')
        expect_document(content, 0)
        # Ctrl+N repeats by line; releasing it leaves the same visible cursor.
        hold(('ctrl', 'n'), .95)
        released = capture('compact-navigation-ctrl-n-released')
        settle(.7)
        stopped = capture('compact-navigation-ctrl-n-stopped')
        assert crop(released, 0, 32, released[0], released[1]-32) == crop(stopped, 0, 32, stopped[0], stopped[1]-32), 'Released Ctrl+N continued moving'
        moved, _ = inspect_document()
        starts = [sum(map(len, lines[:index])) for index in range(2, 26)]
        assert moved[0] == len(content) and moved[1] in starts, moved
        press('meta_l', '2')
        doc_start()
        press('ctrl', 'e')
        hold(('ctrl', 'b'), .9)
        moved, _ = inspect_document()
        assert moved[0] == len(content) and 0 <= moved[1] <= 20, moved
        press('meta_l', '2')
        doc_start()
        hold(('alt', 'f'), .85)
        moved, _ = inspect_document()
        assert moved[0] == len(content) and moved[1] > 10, moved
        press('meta_l', '2')
        doc_start()
        # Save remains a single command even while the chord is held.
        start = len(transcript)
        hold(('ctrl', 's'), .95)
        wait(b'source draft saved;', start, 12)
        assert transcript[start:].count(b'source draft saved;') == 1, 'Held Ctrl+S repeated'
        saved = expect_document(content, 0, dirty=0)
        assert b'source storage generation/restored/blocked/different-base: 1/0/0/0' in transcript[start:]
        # Alt+X stays open on hold; editor chords cannot move or edit its draft.
        hold(('alt', 'x'), .95)
        for chord in [('ctrl', 'n'), ('ctrl', 'p'), ('ctrl', 'v'),
                      ('alt', 'v'), ('alt', 'f'), ('ctrl', 'b')]:
            press(*chord)
        type_text('source inf')
        press('ctrl', 'n')
        press('ctrl', 'p')
        start = len(transcript)
        press('ret')
        wait(b'source bytes/cursor', start, 8)
        intact = expect_document(content, 0, dirty=0)
        assert intact == saved, (saved, intact)
        # Super is global while the palette is open and closes it on selection.
        for key in ('1', '2', '3'):
            press('alt', 'x')
            type_text('layout')
            press('meta_l', key)
            picture = capture('compact-navigation-global-super-' + key)
            assert crop(picture, 0, 32, 160, 48) == signatures[int(key)-1], 'Super layout was not global in palette'
        intact, _ = report(console_command('d', b'source bytes/cursor'))
        assert intact == saved, (saved, intact)
        press('meta_l', '2')
        doc_start()
        press('ctrl', 'u')
        press('3')
        press('ctrl', 'k')
        content = b'\nrow_two\n' + original
        expect_document(content, 0)
        # Selection, cut/yank and per-byte undo remain available on compact keys.
        press('ctrl', 'n')
        press('ctrl', 'spc')
        press('alt', 'f')
        press('ctrl', 'w')
        expect_document(b'\n\n'+original, 1)
        press('ctrl', 'y')
        expect_document(content, 8)
        press('ctrl', 'z')
        expect_document(b'\nrow_tw\n'+original, 7)
        for _ in range(6):
            press('ctrl', 'z')
        expect_document(b'\n\n'+original, 1)
        press('ctrl', 'y')
        expect_document(content, 8)
        # Modified destructive commands execute once rather than auto-repeating.
        doc_start()
        hold(('ctrl', 'k'), .95)
        content = b'row_two\n'+original
        expect_document(content, 0)
        hold(('alt', 'd'), .95)
        content = b'\n'+original
        final = expect_document(content, 0)
        start = len(transcript)
        press('ctrl', 's')
        wait(b'source draft saved;', start, 12)
        final_saved, _ = inspect_document()
        assert final_saved == (len(content), 0, final[2], 0, 0, 0), final_saved
        console_command('q', b'halted')
        qmp('quit')
        assert process.wait(timeout=5) == 0
        image = (tmp / 'state.img').read_bytes()
        banks = [struct.unpack_from('<12Q', image, bank*512) + (bank,) for bank in (1283, 2308)]
        valid = [header for header in banks if header[0] == int.from_bytes(b'MIRASRC3', 'little') and header[1] == 3]
        assert valid, 'No native source bank was written'
        header = max(valid, key=lambda entry: entry[3])
        payload = image[(header[12]+1)*512:(header[12]+1)*512+header[2]]
        assert payload == content and header[3] == 2, 'Saved bytes/generation differ from compact-keyboard edits'
        print('PASS: graphical USB compact navigation/pages/doc ends, right modifiers, Ctrl/Alt navigation repeat and release, one-shot save/palette/deletion, global Super, selection/undo and exact saved bytes')
    except Exception:
        if stream is not None and process.poll() is None:
            try:
                registers = qmp('human-monitor-command', {'command-line': 'info registers'})
                (out/'compact-navigation-failure-registers.txt').write_text(registers)
                qmp('screendump', {'filename': str(out/'compact-navigation-failure.ppm')})
            except (OSError, ValueError, RuntimeError):
                pass
        raise
    finally:
        (out/'compact-navigation.log').write_bytes(transcript)
        if stream is not None:
            if process.poll() is None:
                try: qmp('quit')
                except (OSError, ValueError): pass
            stream.close()
        if sock is not None: sock.close()
        selector.close()
        if process.poll() is None: process.terminate()
        process.wait(timeout=5)
