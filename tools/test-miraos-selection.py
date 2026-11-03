#!/usr/bin/env python3
"""One graphical USB selection/internal-clipboard/save check; serial is output-only."""
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
subprocess.run(['cargo','run','-q','-p','mira-lang','--','build-uefi','projects/MiraOS','target/miraos/selection-esp'],cwd=root,check=True,timeout=120)
out = root / 'target/miraos/smoke'
out.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='mira-selection-') as directory:
    tmp = Path(directory)
    firmware = Path(os.environ.get('MIRA_OVMF_DIR', '/usr/share/edk2/x64'))
    shutil.copyfile(firmware / 'OVMF_VARS.4m.fd', tmp / 'vars.fd')
    with (tmp / 'state.img').open('xb') as state:
        state.truncate(2 * 1024 * 1024)
        state.write(b'MIRAVOL2' + (2).to_bytes(8, 'little'))
    command = ['qemu-system-x86_64', '-accel', 'tcg', '-machine', 'q35', '-m', '128M',
        '-vga', 'std', '-display', 'gtk', '-name', 'MiraOS - selection check', '-nic', 'none', '-monitor', 'none',
        '-drive', f'if=pflash,format=raw,readonly=on,file={firmware}/OVMF_CODE.4m.fd',
        '-drive', f'if=pflash,format=raw,file={tmp}/vars.fd',
        '-drive', f'if=none,id=esp,format=raw,readonly=on,file=fat:{root}/target/miraos/selection-esp',
        '-device', 'virtio-blk-pci,drive=esp,bootindex=1',
        '-device', 'qemu-xhci,id=xhci,addr=04.0',
        '-device', 'usb-kbd,bus=xhci.0,port=1',
        '-blockdev', json.dumps({'driver': 'raw', 'node-name': 'mira-state',
            'file': {'driver': 'file', 'filename': str(tmp / 'state.img')}}),
        '-device', 'virtio-blk-pci,drive=mira-state,addr=06.0,disable-modern=on,vectors=0,queue-size=256,serial=MIRA-EXPERIENCE',
        '-debugcon', f'file:{out}/selection-debug.log', '-serial', 'stdio', '-no-reboot', '-no-shutdown',
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

    def capture(name):
        settle(.2)
        path = out / (name + '.ppm')
        qmp('screendump', {'filename': str(path)})
        with path.open('rb') as picture:
            assert picture.readline().strip() == b'P6'
            width, height = map(int, picture.readline().split())
            assert picture.readline().strip() == b'255'
            pixels = picture.read()
        return width, height, pixels

    def crop(picture, x0, y0, x1, y1):
        width, _, pixels = picture
        return b''.join(pixels[(row * width + x0)*3:(row * width + x1)*3]
                        for row in range(y0, y1))

    def type_text(text):
        for char in text:
            if char == ' ':
                press('spc')
            elif char == '\n':
                press('ret')
            elif char == '_':
                press('shift', 'minus')
            else:
                press(char)

    def inspect_document():
        start = len(transcript)
        press('ctrl', 'x')
        wait(b'source bytes/cursor', start, 8)
        return report(start)

    def expect_document(length, cursor, line, dirty=1):
        observed, actual_line = inspect_document()
        assert (observed[0], observed[1], observed[3:]) == (length, cursor, (dirty, 0, 0)), observed
        assert actual_line == line, (actual_line, line, observed)
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
        original = (root / 'target/miraos/selection-esp/source.mira').read_bytes()
        assert initial == (len(original), 0, 0, 0, 0, 0), initial
        press('meta_l', '2')
        fixture = 'alpha_beta 123 gamma\nrow_two\n'
        type_text(fixture)
        press('ctrl', 'a')
        press('ctrl', 'p')
        press('ctrl', 'p')
        total = initial[0] + len(fixture)
        first_line = b'alpha_beta 123 gamma'
        fixture_report = expect_document(total, 0, first_line)
        # Mark and movement draw a real background over the selected source bytes.
        press('ctrl', 'spc')
        press('ctrl', 'u')
        press('5')
        press('ctrl', 'f')
        selected = capture('selection-forward')
        press('ctrl', 'g')
        cancelled = capture('selection-cancelled')
        marked_pixels = crop(selected, 0, 48, selected[0], 64)
        clear_pixels = crop(cancelled, 0, 48, cancelled[0], 64)
        changed = sum(marked_pixels[index:index+3] != clear_pixels[index:index+3]
                      for index in range(0, len(marked_pixels), 3))
        assert changed > 100, ('No visible source selection background', changed)
        cancelled_report = expect_document(total, 5, first_line)
        assert cancelled_report[2] == fixture_report[2], 'Mark/cancel changed source revision'
        press('ctrl', 'a')
        press('ctrl', 'spc')
        press('ctrl', 'u')
        press('5')
        press('ctrl', 'f')
        press('alt', 'w')
        press('ctrl', 'w')
        copied = expect_document(total, 5, first_line)
        assert copied[2] == fixture_report[2], 'Copy edited the source'
        # The clipboard belongs to Mira and survives view and palette changes.
        for key in ('1', '3', '2'):
            press('meta_l', key)
        press('alt', 'x')
        type_text('layout con')
        press('esc')
        press('ctrl', 'e')
        press('ctrl', 'y')
        appended = first_line + b'alpha'
        yanked = expect_document(total+5, len(appended), appended)
        press('ctrl', 'z')
        one_undo = expect_document(total+4, len(appended)-1, appended[:-1])
        assert one_undo[2] == yanked[2]+1, (yanked, one_undo)
        for _ in range(4):
            press('ctrl', 'z')
        expect_document(total, len(first_line), first_line)
        # Backward region [5,10) cuts the exact underscore-containing suffix.
        press('ctrl', 'a')
        press('ctrl', 'u')
        press('1')
        press('0')
        press('ctrl', 'f')
        press('ctrl', 'spc')
        press('ctrl', 'u')
        press('5')
        press('ctrl', 'b')
        capture('selection-backward')
        press('ctrl', 'w')
        press('ctrl', 'w')
        cut_line = first_line[:5] + first_line[10:]
        expect_document(total-5, 5, cut_line)
        press('ctrl', 'y')
        expect_document(total, 10, first_line)
        # Empty cut preserves the earlier clipboard instead of replacing it.
        press('ctrl', 'spc')
        press('ctrl', 'w')
        expect_document(total, 10, first_line)
        press('ctrl', 'e')
        press('ctrl', 'y')
        appended = first_line + b'_beta'
        expect_document(total+5, len(appended), appended)
        # Ordinary typing inserts at point, clears the mark, and keeps selected bytes.
        press('ctrl', 'a')
        press('ctrl', 'spc')
        press('ctrl', 'u')
        press('5')
        press('ctrl', 'f')
        press('q')
        press('ctrl', 'w')
        typed = appended[:5] + b'q' + appended[5:]
        expect_document(total+6, 6, typed)
        press('ctrl', 'z')
        expect_document(total+5, 5, appended)
        # Ctrl+G cancels both mark and numeric argument, without editing the draft.
        press('ctrl', 'spc')
        press('ctrl', 'u')
        press('3')
        press('ctrl', 'f')
        press('ctrl', 'g')
        press('ctrl', 'u')
        press('9')
        press('ctrl', 'g')
        press('ctrl', 'f')
        press('ctrl', 'w')
        expect_document(total+5, 9, appended)
        press('ctrl', 'y')
        final_line = appended[:9] + b'_beta' + appended[9:]
        final = expect_document(total+10, 14, final_line)
        start = len(transcript)
        press('ctrl', 's')
        wait(b'source draft saved;', start, 12)
        saved, saved_line = inspect_document()
        assert saved == (total+10, 14, final[2], 0, 0, 0) and saved_line == final_line, (saved, saved_line)
        console_command('q', b'halted')
        qmp('quit')
        assert process.wait(timeout=5) == 0
        # Verify every persisted byte, including the untouched original source tail.
        image = (tmp / 'state.img').read_bytes()
        banks = [struct.unpack_from('<12Q', image, bank*512) + (bank,) for bank in (1283, 2308)]
        valid = [header for header in banks if header[0] == int.from_bytes(b'MIRASRC3', 'little') and header[1] == 3]
        assert valid, 'No native source bank was written'
        header = max(valid, key=lambda entry: entry[3])
        expected = final_line + b'\nrow_two\n' + original
        assert (header[2], header[5], header[6]) == (len(expected), 14, final[2]), header
        payload = image[(header[12]+1)*512:(header[12]+1)*512+header[2]]
        assert payload == expected, 'Saved source differs from the USB selection/edit result'
        print('PASS: graphical USB forward/backward mark, visible selection, copy/cut/yank, per-byte undo, cancel/empty mark, clipboard across layouts/palette, exact native saved bytes')
    finally:
        (out/'selection.log').write_bytes(transcript)
        if stream is not None:
            if process.poll() is None:
                try: qmp('quit')
                except (OSError, ValueError): pass
            stream.close()
        if sock is not None: sock.close()
        selector.close()
        if process.poll() is None: process.terminate()
        process.wait(timeout=5)
