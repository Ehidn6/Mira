#!/usr/bin/env python3
"""One focused UEFI run of autonomous activity and its observer console."""
import argparse
import json
import os
from pathlib import Path
import re
import selectors
import signal
import struct
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parents[1]
def project_source():
    return subprocess.check_output([str(root/'target/debug/mira'),'source',str(root/'projects/MiraOS'),'text'],cwd=root,text=True)

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--reject-model", action="store_true", help="fault a temporary neural activation and verify containment")
parser.add_argument("--persistence", action="store_true", help="restart, damage the newest slot, and check revision rejection")
parser.add_argument("--editor", action="store_true", help="edit actual source through the native UART input path")
parser.add_argument("--source-check", action="store_true", help="check edited source with the native Mira compiler scanner")
parser.add_argument("--graph", action="store_true", help="navigate native nested spaces, ports and incident links")
options = parser.parse_args()
assert sum((options.persistence, options.reject_model, options.editor, options.source_check, options.graph)) <= 1
disk_fixture = tempfile.TemporaryDirectory(prefix="miraos-state-test-")
fixture = tempfile.TemporaryDirectory(prefix="miraos-reject-") if options.reject_model else None
if fixture:
    directory = Path(fixture.name)
    source = project_source()
    source, count = re.subn(r"(?m)^(  activate(?: @\d+)?\s*\([^\n]*?=)[^\n]*", r"\1 1.0 / 0.0", source)
    assert count == 1
    (directory / "kernel.mira").write_text(source)
    command = ["cargo", "run", "--quiet", "-p", "mira-lang", "--", "run-uefi", str(directory / "kernel.mira"), str(directory / "esp"), "--headless"]
else:
    command = [str(root / "run-miraos.sh")]
environment = dict(os.environ)
environment["MIRA_STATE_DISK"] = str(Path(disk_fixture.name) / "experience.img")
process = subprocess.Popen(command, env=environment, cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
selector = selectors.DefaultSelector()
selector.register(process.stdout, selectors.EVENT_READ)
transcript = bytearray()


def wait_for(token, offset=0, seconds=25):
    deadline = time.monotonic() + seconds
    while token not in transcript[offset:]:
        if process.poll() is not None or time.monotonic() > deadline:
            raise RuntimeError("MiraOS did not reach expected state:\n" + transcript.decode(errors="replace")[-3500:])
        for key, _ in selector.select(0.05):
            transcript.extend(key.fileobj.read1(8192))
    return transcript.index(token, offset)


def exchange(line, marker):
    offset = len(transcript)
    process.stdin.write(line.encode() + b"\n")
    process.stdin.flush()
    found = wait_for(marker, offset, 8)
    wait_for(b"ready> ", found, 8)
    return bytes(transcript[offset:]).decode(errors="replace")


def status():
    text = exchange(":s", b"attempts/steps/observations/external:")
    observed = re.search(r"attempts/steps/observations/external: (\d+)/(\d+)/(\d+)/(\d+)", text)
    learned = re.search(r"updates/replays/recalls/memory: (\d+)/(\d+)/(\d+)/(\d+)", text)
    control = re.search(r"paused/quarantined/failures/pending/dropped: (\d+)/(\d+)/(\d+)/(\d+)/(\d+)", text)
    values = list(map(int, observed.groups() + learned.groups() + control.groups()))
    return dict(zip(("attempts","steps","observations","external","updates","replays","recalls","memory","paused","quarantined","failures","pending","dropped"), values))


def stop_vm():
    offset = len(transcript)
    process.stdin.write(b":q\n")
    process.stdin.flush()
    wait_for(b"halted",offset,12)
    process.stdin.write(b"\x01x")
    process.stdin.flush()
    assert process.wait(timeout=5) == 0
    selector.unregister(process.stdout)


def restart(next_command=None):
    global process
    offset = len(transcript)
    process = subprocess.Popen(next_command or command,env=environment,cwd=root,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,start_new_session=True)
    selector.register(process.stdout,selectors.EVENT_READ)
    wait_for(b"ready> ",offset)


def storage_status(text):
    found = re.findall(r"storage bound/generation/restored/saves/rejected/foreign: (\d+)/(\d+)/(\d+)/(\d+)/(\d+)/(\d+)",text)
    assert found,text
    return tuple(map(int,found[-1]))


def until(predicate):
    deadline = time.monotonic() + 8
    while True:
        time.sleep(0.05)  # No commands that start work are sent.
        current = status()
        if predicate(current):
            return current
        if time.monotonic() >= deadline:
            raise AssertionError(f"autonomous progress missing: {current}")


try:
    wait_for(b"ready> ")
    if options.graph:
        exchange(":p",b"autonomy pause changed")
        before=status()
        first=exchange(":g",b"Graph path:")
        assert "Graph path: /MiraOS/kernel/Runtime" in first,first
        assert "space ID/cells/links: 101/25/2" in first,first
        assert "115 rate_value in/out 1/1" in first and "121 document" not in first,first
        second=exchange(":n",b"Graph path:")
        assert "Cells page 2" in second and "121 document in/out 0/1" in second,second
        previous=exchange(":m",b"Graph path:")
        assert "Cells page 1" in previous,previous
        info=exchange(":i 115",b"Node 115")
        assert "0/0 value f32" in info and "1/0 out f32" in info,info
        assert "114 base.out -> 115 rate_value.value" in info,info
        assert "115 rate_value.out -> 101 Runtime.rate" in info,info
        boundary=exchange(":i 101",b"Node 101")
        assert "1/0 rate f32" in boundary and "Runtime.rate" in boundary,boundary
        exchange(":z 115",b"Choose a cell with an inner space")
        assert "101/25/2" in exchange(":g",b"Graph path:")
        root_view=exchange(":o",b"Graph path:")
        assert "space ID/cells/links: 1/1/0" in root_view and "100 kernel" in root_view,root_view
        exchange(":o",b"Already at the root")
        exchange(":z 999",b"Choose a cell with an inner space")
        nested=exchange(":z 100",b"Graph path:")
        assert "101/25/2" in nested,nested
        for argument in ("0","-1","115junk","999999999999999999999999999999999999"):
            exchange(":i "+argument,b"Choose a cell or boundary")
        record=exchange(":i 121",b"Node 121")
        assert "out data type-id" in record,record
        after=status()
        assert before["steps"]==after["steps"] and before["external"]==after["external"]==0
    elif options.source_check:
        exchange(":p",b"autonomy pause changed")
        before=status()
        def check(expected,line=None,column=None):
            result=exchange(":v",b"source check revision/status/line/column/tokens:")
            values=tuple(map(int,re.search(r"source check revision/status/line/column/tokens: ([0-9/]+)",result).group(1).split("/")))
            assert values[1]==expected,(values,result)
            if line is not None: assert values[2]==line,(values,result)
            if column is not None: assert values[3]==column,(values,result)
            return values
        def edit(keys):
            offset=len(transcript)
            process.stdin.write(b":b\n");process.stdin.flush()
            wait_for(b"Source draft:",offset)
            time.sleep(.08)
            for chunk in (keys[i:i+8] for i in range(0,len(keys),8)):
                process.stdin.write(chunk);process.stdin.flush();time.sleep(.04)
            offset=len(transcript)
            process.stdin.write(b"\x18");process.stdin.flush()
            wait_for(b"ready> ",offset)
        good=check(2)
        assert good[4]>1000,good
        edit(b"]")
        check(8,1,1)
        offset=len(transcript)
        process.stdin.write(b":f\n");process.stdin.flush()
        wait_for(b"Source draft:",offset)
        time.sleep(.08)
        process.stdin.write(b'\x1a"\x12');process.stdin.flush()
        found=wait_for(b"Unclosed text literal.",offset,12)
        output=bytes(transcript[offset:found]).decode(errors="replace")
        assert re.search(r"source check revision/status/line/column/tokens: \d+/4/1/1/",output),output
        offset=len(transcript)
        process.stdin.write(b"\x18");process.stdin.flush()
        wait_for(b"ready> ",offset)
        edit(b'\x1a`a b`')
        check(10,1,1)
        edit(b'\x1a'*5+b'"[(]"')
        check(2)
        edit(b'\x1a'*5+b'([)]')
        check(8,1,3)
        edit(b'\x1a'*4+b'(')
        check(7,1,1)
        edit(b'\x1a')
        check(2)
        after=status()
        assert before["external"]==after["external"]==0 and before["steps"]==after["steps"]
    elif options.editor:
        exchange(":p", b"autonomy pause changed")
        before = status()
        def document():
            output = exchange(":d",b"source bytes/cursor/revision/dirty/full/rejected:")
            match = re.search(r"source bytes/cursor/revision/dirty/full/rejected: ([0-9/]+)",output)
            line = re.search(r"source line: ([^\n]*)",output).group(1)
            return tuple(map(int,match.group(1).split("/"))),line
        original,original_line = document()
        assert original[1:]==(0,0,0,0,0),original
        def enter_editor():
            offset=len(transcript)
            process.stdin.write(b":b\r\n");process.stdin.flush()
            wait_for(b"Source draft:",offset)
            time.sleep(.1)
        def keys(data):
            # Small writes keep this hardware UART check within its real FIFO.
            process.stdin.write(data);process.stdin.flush();time.sleep(.08)
        def leave_editor():
            offset=len(transcript)
            process.stdin.write(b"\x18");process.stdin.flush()
            wait_for(b"ready> ",offset,8)
        enter_editor()
        keys(b"abc\r\nxy\r\n")
        keys(b"\x1b[A\x1b[C")
        keys(b"\x1b[3~\x1a")
        keys(b"Z\x7f\x1b[H\x1b[F")
        keys(b"\r\n\x1a\t\x1a\x1a")
        # Unsupported modified CSI keys must not insert their parameter bytes.
        keys(b"\x1b[1;5D")
        leave_editor()
        edited,line=document()
        assert edited[0]==original[0]+7 and edited[1]==6 and edited[3:]==(1,0,0),(edited,line)
        assert line=="xy",line
        after=status()
        assert all(after[k]==before[k] for k in ("steps","external","pending")),(before,after)
        exchange(":q",b"Unsaved source draft:")
        assert process.poll() is None
        enter_editor()
        keys(b"\x1a"*9)
        leave_editor()
        restored,line=document()
        assert restored[0]==original[0] and restored[1]==0 and restored[3:]==(0,0,0),(restored,line)
        assert restored[2]>edited[2] and line==original_line,(restored,line)
        exchange(":p",b"autonomy pause changed")
        until(lambda state: state["steps"]>before["steps"])
        exchange(":p",b"autonomy pause changed")
        original_bytes=(root/"target/miraos/esp/source.mira").read_bytes()
        enter_editor()
        keys(b"// saved A\n")
        offset=len(transcript)
        process.stdin.write(b"\x13");process.stdin.flush()
        wait_for(b"source draft saved;",offset,12)
        leave_editor()
        assert document()[0][3]==0
        enter_editor()
        keys(b"// saved B\n")
        leave_editor()
        exchange(":w",b"source draft saved;")
        expected=b"// saved A\n// saved B\n"+original_bytes
        stop_vm()
        path=Path(environment["MIRA_STATE_DISK"])
        image=bytearray(path.read_bytes())
        banks=[]
        for bank in (1283,2308):
            header=struct.unpack_from("<12Q",image,bank*512)
            assert header[0]==int.from_bytes(b"MIRASRC3","little") and header[1]==3,header
            banks.append((header[3],bank,header[2]))
        banks.sort()
        _,newest,length=banks[-1]
        assert image[(newest+1)*512:(newest+1)*512+length]==expected
        restart()
        restored,_=document()
        assert restored[0]==len(expected) and restored[1]==22 and restored[3]==0,restored
        assert "source storage generation/restored/blocked/different-base: 2/1/0/0" in exchange(":d",b"source storage generation/")
        stop_vm()
        image=bytearray(path.read_bytes())
        image[(newest+1)*512] ^= 1
        path.write_bytes(image)
        restart()
        older,_=document()
        assert older[0]==len(original_bytes)+11 and older[1]==11 and older[3]==0,older
        assert "source storage generation/restored/blocked/different-base: 1/1/0/0" in exchange(":d",b"source storage generation/")
    elif options.reject_model:
        fault = until(lambda s: s["quarantined"] == 1)
        assert fault["attempts"] == fault["failures"] == 3, fault
        assert fault["steps"] == fault["updates"] == 0, fault
        assert fault["observations"] == fault["memory"] == 1, fault
        exchange("hello", b"input event queued")
        retained = status()
        assert retained["quarantined"] == 1 and retained["pending"] == 1, retained
        memory = exchange(":e", b"memory: origin/subject/tick/error/payload;")
        assert "/-1.000/" in memory, memory
    elif options.persistence:
        until(lambda s:s["steps"]>=2)
        exchange("hello",b"input event queued")
        until(lambda s:s["external"]==1)
        exchange(":p",b"autonomy pause changed")
        held = status()
        assert held["paused"]==1 and held["external"]==1,held
        exchange(":k",b"checkpoint saved")
        stop_vm()
        path = Path(environment["MIRA_STATE_DISK"])
        image = bytearray(path.read_bytes())
        slots=[]
        for bank in (1,129):
            words=struct.unpack_from("<10Q",image,bank*512)
            assert words[0]==int.from_bytes(b"MIRACKP1","little")
            slots.append((words[3],bank,words))
        slots.sort()
        inspected=json.loads(subprocess.check_output(["cargo","run","--quiet","-p","mira-lang","--","inspect",str(root/"projects/MiraOS"),"enter"],cwd=root))
        native_revision=b"".join(word.to_bytes(8,"little") for word in slots[-1][2][5:9]).hex()
        assert native_revision==inspected["revision"],(native_revision,inspected["revision"])
        previous,newest=slots
        assert newest[0]>previous[0]>0,slots
        image[newest[1]*512+512+8] ^= 1
        path.write_bytes(image)
        restart()
        restored=status()
        assert restored["paused"]==1,restored
        assert all(restored[k]==held[k] for k in ("steps","updates","observations","external","replays","recalls","memory")),(held,restored)
        state=storage_status(exchange(":s",b"storage bound/"))
        assert state[0]==1 and state[2]==previous[0] and state[4]>=1 and state[5]==0,state
        assert "/hello\n" in exchange(":e",b"memory: origin/subject/tick/error/payload;")
        stop_vm()
        intact=path.read_bytes()
        # Same shape, deliberately different code: its Inspect revision differs.
        directory=Path(disk_fixture.name)
        changed=re.sub(r"(type Payloads(?: @\d+)?\s*=\s*\[i64,)2048", r"\g<1>2049", re.sub(r"(rate_base(?: @\d+)?\s*\(\)->f32\s*=\s*)0\.02\b", r"\g<1>0.021", project_source()))
        assert changed != project_source()
        source=directory/"changed.mira"
        source.write_text(changed)
        changed_command=["cargo","run","--quiet","-p","mira-lang","--","run-uefi",str(source),str(directory/"changed-esp"),"--headless"]
        restart(changed_command)
        fresh=status()
        version=storage_status(exchange(":s",b"storage bound/"))
        assert fresh["external"]==0 and version[2]==0 and version[5]==1,(fresh,version)
        stop_vm()
        assert path.read_bytes()==intact,"different program revision overwrote old state"
        # Also check a mixed volume: this program's older valid slot and a
        # newer, correctly committed slot from another canonical revision.
        image=bytearray(path.read_bytes())
        mixed=[]
        for bank in (1,129):
            words=list(struct.unpack_from("<10Q",image,bank*512))
            mixed.append((words[3],bank,words))
        mixed.sort()
        own,newer=mixed
        newer[2][5] ^= 1
        digest=0x14650fb0739d0383
        for word in newer[2][:9]:
            digest=((digest ^ word)*1099511628211)&((1<<64)-1)
        newer[2][9]=digest
        struct.pack_into("<10Q",image,newer[1]*512,*newer[2])
        path.write_bytes(image)
        intact=bytes(image)
        restart()
        mixed_state=storage_status(exchange(":s",b"storage bound/"))
        assert mixed_state[2]==own[0] and mixed_state[5]==1,mixed_state
        stop_vm()
        assert path.read_bytes()==intact,"loading own bank cleared protection of a foreign bank"
        print("PASS: durable state, damaged-newest recovery, schema/revision identity, mixed-bank write protection")
    else:
        first = until(lambda s: s["steps"] >= 2 and s["replays"] > 0)
        second = until(lambda s: s["steps"] > first["steps"] and s["updates"] > first["updates"])
        assert second["external"] == 0 and second["observations"] > 0, second
        assert second["failures"] == second["quarantined"] == 0, second
        exchange(":p", b"autonomy pause changed")
        paused = status()
        assert paused["paused"] == 1
        time.sleep(0.1)
        still = status()
        assert all(still[k] == paused[k] for k in ("steps","updates","replays","observations")), (paused,still)
        exchange("hello", b"input event queued")
        queued = status()
        assert queued["pending"] == 1 and queued["external"] == 0, queued
        exchange(":p", b"autonomy pause changed")
        consumed = until(lambda s: s["external"] == 1)
        assert consumed["pending"] == 0 and consumed["steps"] > paused["steps"], consumed
        exchange(":p", b"autonomy pause changed")
        memory = exchange(":e", b"memory: origin/subject/tick/error/payload;")
        assert "/hello\n" in memory, memory
        exchange(":r", b"neural state and experience reset")
        reset = status()
        assert reset["paused"] == 1 and reset["memory"] == reset["steps"] == reset["updates"] == 0, reset
        exchange(":p", b"autonomy pause changed")
        restarted = until(lambda s: s["steps"] > 0)
        assert restarted["quarantined"] == restarted["failures"] == 0, restarted
    if not options.persistence:
        stop_vm()
    assert b"MIRA TRAP" not in transcript and b"trial FIFO" not in transcript
    if not options.persistence:
        print("PASS: native graph nesting, paging, typed ports, boundary links and invalid-ID handling" if options.graph else "PASS: native compiler source scan, precise errors, error navigation, undo, no neural input pollution" if options.source_check else "PASS: native editing/undo and neural execution; source save/restart and damaged-newest recovery" if options.editor else "PASS: contained faulty activation; observations and input retained" if fixture else "PASS: autonomous recurrence/learning/replay, real input, observer pause/reset/resume")
finally:
    log = root / "target/miraos/smoke" / ("graph.log" if options.graph else "source-check.log" if options.source_check else "editor.log" if options.editor else "persistence.log" if options.persistence else "rejected-model.log" if fixture else "serial.log")
    log.parent.mkdir(parents=True, exist_ok=True)
    log.write_bytes(transcript)
    selector.close()
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
    if fixture:
        fixture.cleanup()

    disk_fixture.cleanup()
