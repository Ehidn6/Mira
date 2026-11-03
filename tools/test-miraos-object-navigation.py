#!/usr/bin/env python3
"""One graphical USB selected-cell/shared-source navigation check; serial is output-only."""
import json
import os
from pathlib import Path
import re
import sys
import selectors
import shutil
import socket
import struct
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parents[1]
source = sys.argv[1] if len(sys.argv) > 1 else 'projects/MiraOS'
if '--prepared' not in sys.argv:
    subprocess.run(['target/debug/mira','build-uefi',source,'target/miraos/object-navigation-test-esp'],cwd=root,check=True,timeout=180)
image = root / os.environ.get('MIRA_TEST_ESP', 'target/miraos/object-navigation-test-esp')
out = root / 'target/miraos/smoke'
out.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='mira-object-navigation-') as directory:
    tmp = Path(directory)
    firmware = Path(os.environ.get('MIRA_OVMF_DIR', '/usr/share/edk2/x64'))
    shutil.copyfile(firmware / 'OVMF_VARS.4m.fd', tmp / 'vars.fd')
    with (tmp / 'state.img').open('xb') as state:
        state.truncate(2 * 1024 * 1024)
        state.write(b'MIRAVOL2' + (2).to_bytes(8, 'little'))
    command = ['qemu-system-x86_64', '-accel', 'tcg', '-machine', 'q35', '-m', '128M',
        '-vga', 'std', '-display', 'gtk', '-name', 'MiraOS - object navigation check', '-nic', 'none', '-monitor', 'none',
        '-drive', f'if=pflash,format=raw,readonly=on,file={firmware}/OVMF_CODE.4m.fd',
        '-drive', f'if=pflash,format=raw,file={tmp}/vars.fd',
        '-drive', f'if=none,id=esp,format=raw,readonly=on,file=fat:{image}',
        '-device', 'virtio-blk-pci,drive=esp,bootindex=1',
        '-device', 'qemu-xhci,id=xhci,addr=04.0',
        '-device', 'usb-kbd,bus=xhci.0,port=1',
        '-blockdev', json.dumps({'driver': 'raw', 'node-name': 'mira-state',
            'file': {'driver': 'file', 'filename': str(tmp / 'state.img')}}),
        '-device', 'virtio-blk-pci,drive=mira-state,addr=06.0,disable-modern=on,vectors=0,queue-size=256,serial=MIRA-EXPERIENCE',
        '-debugcon', f'file:{out}/object-navigation-debug.log', '-serial', 'stdio', '-no-reboot', '-no-shutdown',
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
    deadline = time.monotonic() + 240

    def pump(seconds=.05):
        if time.monotonic() > deadline:
            raise RuntimeError('USB keyboard test exceeded 240 seconds')
        for key, _ in selector.select(seconds):
            transcript.extend(key.fileobj.read1(8192))

    def wait(marker, start=0, seconds=30):
        end = min(deadline, time.monotonic() + seconds)
        while marker not in transcript[start:]:
            if process.poll() is not None or time.monotonic() > end:
                if process.poll() is None and stream is not None:
                    try:
                        qmp('screendump', {'filename': str(out / 'object-navigation-failed.ppm')})
                    except (OSError, RuntimeError):
                        pass
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
            settle(.12 if '--runner-only' in sys.argv else .07)

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

    def capture(name):
        path = out / (name + '.ppm')
        previous = None
        for _ in range(30):
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
            '=': ('equal',), '.': ('dot',), '-': ('minus',), '>': ('shift', 'dot'), '/': ('slash',),
            '@': ('shift','2'), '*': ('shift','8'), '+': ('shift','equal'), ',': ('comma',)}
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

    def graph_counts():
        start = console_command('g', b'space ID/cells/links:')
        wait(b'ready> ', start, 8)
        found = re.search(rb'space ID/cells/links: (\d+)/(\d+)/(\d+)', transcript[start:])
        assert found, 'Missing live graph counts'
        return tuple(map(int, found.groups()))

    def navigation_pane(picture):
        width, _, _ = picture
        return crop(picture, 0, 48, min(width//8//6, 32)*8, 320)

    def beginning(cursor):
        for _ in range((cursor+999)//1000):
            press('ctrl', 'u')
            type_text('1000')
            press('ctrl', 'b')

    try:
        connect_keyboard()
        # GTK remains visible, while physical host typing returns to its prior window.
        # QMP injects into the guest independently of the host window's focus.
        if previous_focus:
            subprocess.run(['hyprctl', 'dispatch',
                f'hl.dsp.focus({{window="address:{previous_focus}"}})'],
                check=True, stdout=subprocess.DEVNULL, timeout=5)
        if '--runner-only' in sys.argv:
            original = (image / 'source.mira').read_bytes()
            matched = re.search(rb'(?m)^([ ]*)layout @231',original)
            assert matched, 'Missing existing selected source function'
            prefix = matched.group(1).decode()

            def action(query):
                press('alt','x')
                type_text(query)
                start = len(transcript)
                press('ret')
                return start

            start = action('source open 231')
            wait(b'SOURCE:',start,15)

            def replace_object(body, result_type='i64'):
                press('alt','shift','comma')
                press('ctrl','spc')
                press('alt','shift','dot')
                press('ctrl','w')
                text = prefix + 'layout @231()->' + result_type + ' = ' + body
                type_text(text)

            replace_object('6 * 7\n')
            start = action('source run')
            wait(b'RUN result: 42',start,30)
            capture('native-editor-run-42')
            start = action('source run')
            wait(b'RUN result: 42',start,10)
            assert b'COMPILE:' not in transcript[start:] and b'COMPILE ready' not in transcript[start:], 'Unchanged source was recompiled'
            replace_object('7 * 8\n')
            start = action('source run')
            wait(b'RUN result: 56',start,30)
            replace_object('1 / 0\n')
            start = action('source run')
            wait(b'RUN stopped: division by zero',start,30)
            # A bounded infinite loop must return control to the editor.
            replace_object('\n'+prefix+'  while true\n'+prefix+'    ()\n'+prefix+'  0\n')
            start = action('source run')
            wait(b'RUN stopped: operation budget exhausted',start,30)
            replace_object('sys.load64(sys.addr(0))\n')
            start = action('source run')
            wait(b'COMPILE rejected: raw addresses',start,30)
            capture('native-editor-run-rejected')
            replace_object('10000000.0\n','f32')
            start = action('source run')
            wait(b'RUN result: 1.000e7',start,30)
            # A rejected input cannot poison the next edit/compilation.
            replace_object('40 + 2\n')
            start = action('source run')
            wait(b'RUN result: 42',start,30)
            print('PASS: native editor edit/compile/run, cache, revision invalidation, division/fuel errors, raw-access rejection and recovery')
            qmp('quit')
            assert process.wait(timeout=5) == 0
            sys.exit(0)
        if '--ui-only' in sys.argv:
            initial_picture = capture('workspace-output-hidden')
            width, height, _ = initial_picture
            columns, rows = width // 8, height // 16
            boundary = min(columns // 6, 32) * 8
            lower = (rows - 6) * 16
            border = crop(initial_picture, boundary, lower, boundary + 1, lower + 1)
            assert border != b'\0\0\0', 'Workspace still reserves a bottom console pane'
            type_text('MiXeD')
            input_picture = capture('workspace-input-overlay')
            assert crop(input_picture, width//3,64,width*2//3,320) != crop(initial_picture,width//3,64,width*2//3,320)
            press('esc')
            cancelled = capture('workspace-input-cancelled')
            assert crop(cancelled,boundary,lower,boundary+1,lower+1) == border
            # Input is submitted through the same dispatcher, not inserted in source.
            start = len(transcript)
            type_text('hello')
            press('ret')
            wait(b'input event queued', start, 8)
            output_picture = capture('workspace-output-on-demand')
            assert crop(output_picture,boundary,lower,boundary+1,lower+1) != border
            press('esc')
            hidden = capture('workspace-output-dismissed')
            assert crop(hidden,boundary,lower,boundary+1,lower+1) == border
            press('alt','x')
            type_text('source open 327')
            press('ret')
            wait(b'SOURCE:', start, 15)
            start = len(transcript)
            press('ctrl','x')
            before, before_line = report(start)
            press('meta_l','2')
            press('alt','x')
            type_text('console input')
            press('ret')
            capture('editor-input-overlay')
            start = len(transcript)
            type_text(':d')
            press('ret')
            wait(b'source line:', start, 8)
            found = re.search(rb'source bytes/cursor/revision/dirty/full/rejected: (\d+)/(\d+)/(\d+)/(\d+)/(\d+)/(\d+)', transcript[start:])
            assert found and tuple(map(int,found.groups())) == before, 'Palette input changed editor context'
            capture('editor-output-on-demand')
            press('esc')
            press('ctrl','n')
            start = len(transcript)
            press('ctrl','x')
            after, _ = report(start)
            assert after[0] == before[0] and after[1] > before[1] and after[2:] == before[2:], 'Editor focus/cursor was lost'
            print('PASS: graphical USB input overlay, cancel, on-demand output/Escape, editor context and cursor')
            qmp('quit')
            assert process.wait(timeout=5) == 0
            sys.exit(0)
        original = (image / 'source.mira').read_bytes()
        initial, _ = report(console_command('d', b'source bytes/cursor'))
        assert initial == (len(original), 0, 0, 0, 0, 0), initial

        def scope(start):
            found = re.search(rb'source object/parent/range: (\d+)/(\d+)/(\d+)/(\d+)',transcript[start:])
            assert found, bytes(transcript[start:])
            return tuple(map(int,found.groups()))

        def info():
            start = console_command('d', b'source bytes/cursor')
            return report(start), scope(start)

        def open_id(identity):
            console_command('O '+str(identity), b'SOURCE:')

        def palette(query):
            press('alt','x')
            type_text(query)
            press('tab')
            start = len(transcript)
            press('ret')
            wait(b'SOURCE:',start,15)

        # The real bootstrap compiler lives in this same editable source tree.
        open_id(258)
        inspect_document()
        (_, _), compiler_scope = info()
        assert compiler_scope[:2] == (258,1),compiler_scope
        open_id(327)
        _, compiler_line = inspect_document()
        (_, _), parser_scope = info()
        assert parser_scope[:2] == (327,258),parser_scope
        assert b'scalar_parse_from @327' in compiler_line,compiler_line
        palette('source parent')
        inspect_document()
        (_, _), compiler_scope = info()
        assert compiler_scope[0] == 258,compiler_scope
        counts = graph_counts()
        console_command('z 259', b'space ID/cells/links:')
        console_command('z 261', b'space ID/cells/links:')
        counts = graph_counts()
        console_command('i 102', b'Node 102 state')
        console_command('c', b'SOURCE:')
        anchored = capture('object-navigation-contained-state')
        opened, line = inspect_document()
        start = len(transcript)
        (opened,line), selected_scope = info()
        assert selected_scope[:2] == (33,63),selected_scope
        offset = selected_scope[2]
        assert line.startswith(b'  state @33 '),line
        # Compact keyboard document boundaries refer to this source object.
        press('meta_l','2')
        press('alt','shift','comma')
        press('ctrl','b')
        view,_ = inspect_document()
        assert view[1] == offset,view
        press('meta_l','2')
        press('alt','shift','dot')
        press('delete')
        view,_ = inspect_document()
        assert view[0] == len(original) and view[1] == selected_scope[3],view
        press('meta_l','2')
        press('alt','shift','comma')
        press('ctrl','e')
        insertion = original.index(b'\n',offset)
        note = b' // edit'
        type_text(note.decode())
        edited,_ = inspect_document()
        expected = original[:insertion]+note+original[insertion:]
        assert edited[:4] == (len(expected),insertion+len(note),len(note),1),edited
        # Return to a prior object with its cursor and dirty state preserved.
        open_id(45)
        other,_ = inspect_document()
        assert other[1] != edited[1]
        palette('source open 33 state')
        again,_ = inspect_document()
        assert again == edited,(again,edited)
        # Global Undo opens the object owning the edit, even from another object.
        open_id(45)
        press('ctrl','z')
        undone,_ = inspect_document()
        assert undone[:4] == (len(expected)-1,insertion+len(note)-1,len(note)+1,1),undone
        (undone,_), where = info()
        assert where[0] == 33,where
        expected = original[:insertion]+note[:-1]+original[insertion:]
        # Parent, children and project actions all use this same source hierarchy.
        palette('source parent')
        inspect_document()
        (_, _),where = info()
        assert where[0] == 63,where
        start = console_command('C',b'SOURCE children of ')
        wait(b'ready> ',start,10)
        assert b'33 state' in transcript[start:]
        palette('source project')
        inspect_document()
        (_, _),where = info()
        assert where[:2] == (1,0),where
        assert graph_counts() == counts,'Source editing changed the running graph'
        # Saving a part checkpoints the full project, including untouched siblings.
        open_id(33)
        start = len(transcript)
        press('ctrl','s')
        wait(b'source draft saved;',start,15)
        saved,_ = inspect_document()
        assert saved[0] == len(expected) and saved[3] == 0,saved
        final_picture = capture('object-navigation-saved-part')
        console_command('q', b'halted')
        qmp('quit')
        assert process.wait(timeout=5) == 0
        image = (tmp/'state.img').read_bytes()
        banks = [struct.unpack_from('<12Q',image,bank*512)+(bank,) for bank in (1283,2308)]
        valid = [h for h in banks if h[0] == int.from_bytes(b'MIRASRC3','little') and h[1] == 3]
        assert valid,'No saved source bank'
        header = max(valid,key=lambda h:h[3])
        payload = image[(header[12]+1)*512:(header[12]+1)*512+header[2]]
        assert payload == expected,'A contained-source edit changed or dropped sibling source'
        # Restart from exactly the saved disk. No live guest/user disk is touched.
        process = subprocess.Popen(command,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
        selector.close();selector=selectors.DefaultSelector();selector.register(process.stdout,selectors.EVENT_READ)
        if stream is not None:stream.close()
        if sock is not None:sock.close()
        stream=None;sock=None
        restart=len(transcript)
        connect_keyboard(restart)
        assert b'source storage generation/restored/blocked/different-base: 1/1/0/0' in transcript[restart:]
        open_id(33)
        restored,restored_line=inspect_document()
        assert restored[0] == len(expected) and restored[3] == 0,restored
        assert note[:-1] in restored_line,restored_line
        print('PASS: embedded compiler/parser source objects, graphical USB source-object open/parent/project, scope bounds, palette selection, cursor/dirty preservation, cross-object Undo, exact whole-project save, and restart restore')
    finally:
        (out / 'object-navigation-serial.log').write_bytes(transcript)
        (out/'object-navigation.log').write_bytes(transcript)
        if stream is not None:
            if process.poll() is None:
                try: qmp('quit')
                except (OSError, ValueError): pass
            stream.close()
        if sock is not None: sock.close()
        selector.close()
        if process.poll() is None: process.terminate()
        process.wait(timeout=5)
