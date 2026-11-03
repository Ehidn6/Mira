#!/usr/bin/env python3
"""Reuse an exact prepared MiraOS image; never rebuild on an unchanged launch."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
image = root / 'target/miraos/esp'
stamp = root / 'target/miraos/prepared.json'
assets = ('EFI/BOOT/BOOTX64.EFI', 'kernel.bin', 'font.bin', 'source.mira')


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def main():
    if sys.argv[1:] not in ([], ['--rebuild']):
        raise SystemExit('usage: prepare-miraos.py [--rebuild]')
    # Cargo tracks embedded compiler/boot assets as well as Rust dependencies.
    subprocess.run(['cargo', 'build', '--quiet', '--offline', '--release', '-p', 'mira-lang', '--bin', 'mira'], cwd=root, check=True)
    binary = root / 'target/release/mira'
    inputs = {'compiler': digest(binary)}
    inputs['projects/MiraOS'] = digest(root / 'projects/MiraOS')
    try:
        previous = json.loads(stamp.read_text())
        reusable = (previous.get('inputs') == inputs and
                    previous.get('assets') == {name: digest(image / name) for name in assets})
    except (OSError, ValueError):
        reusable = False
    if reusable and '--rebuild' not in sys.argv:
        print('MiraOS: prepared image unchanged; starting without recompilation.', flush=True)
        return
    print('MiraOS: source changed or image missing; compiling before launch...', flush=True)
    # A failed build cannot leave an old stamp advertising a usable mixed image.
    stamp.unlink(missing_ok=True)
    subprocess.run([str(binary), 'build-uefi', 'projects/MiraOS', str(image)], cwd=root, check=True)
    record = {'inputs': inputs, 'assets': {name: digest(image / name) for name in assets}}
    temporary = stamp.with_suffix('.tmp')
    temporary.write_text(json.dumps(record, sort_keys=True) + '\n')
    temporary.replace(stamp)


if __name__ == '__main__':
    try:
        main()
    except (OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f'MiraOS preparation failed: {error}')
