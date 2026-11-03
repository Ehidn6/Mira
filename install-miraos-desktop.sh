#!/bin/sh
# Install this checkout's native MiraOS launcher for the current desktop user.
set -eu
MIRA_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
MIRA_APPS=${XDG_DATA_HOME:-$HOME/.local/share}/applications
mkdir -p "$MIRA_APPS"
python3 - "$MIRA_ROOT" "$MIRA_APPS/miraos-native.desktop" <<'PY'
import pathlib
import sys

root, output = map(pathlib.Path, sys.argv[1:])
launcher = root / 'launch-miraos.sh'

def value(text):
    return str(text).replace('\\', '\\\\').replace('\n', '\\n').replace('\r', '\\r').replace('\t', '\\t')

def argument(text):
    # Desktop Exec has its own quoting rules and percent field codes.
    escaped = str(text).replace('%', '%%')
    for character in ('\\', '"', '`', '$'):
        escaped = escaped.replace(character, '\\' + character)
    return value('"' + escaped + '"')

entries = [
    ('miraos-native.desktop', 'MiraOS', 'Start native MiraOS in QEMU',
     'Запустить нативную MiraOS', '--screen', 'computer'),
    ('miraos-context.desktop', 'MiraOS — инструкция', 'Open Mira project context and instructions',
     'Инструкция, команды и синтаксис Mira', '--context', 'help-browser'),
    ('miraos-export-source.desktop', 'MiraOS — экспорт исходника', 'Export the source saved inside MiraOS',
     'Извлечь сохранённый исходник в файл для ChatGPT', '--export', 'document-save'),
]
for filename, name, comment, russian_comment, option, icon in entries:
    entry = '\n'.join([
        '[Desktop Entry]', 'Version=1.0', 'Type=Application',
        f'Name={name}', f'Comment={comment}', f'Comment[ru]={russian_comment}',
        f'Exec={argument(launcher)} {option}', f'TryExec={value(launcher)}',
        f'Path={value(root)}', f'Icon={icon}', 'Terminal=false',
        'Categories=System;Emulator;', 'Keywords=Mira;MiraOS;Native;QEMU;',
        'StartupNotify=false', '',
    ])
    destination = output.parent / filename
    temporary = destination.with_suffix('.desktop.tmp')
    temporary.write_text(entry, encoding='utf-8')
    temporary.chmod(0o644)
    temporary.replace(destination)
    print(f'Installed: {destination}')

PY
if command -v desktop-file-validate >/dev/null 2>&1; then
  for MIRA_ENTRY in miraos-native.desktop miraos-context.desktop miraos-export-source.desktop; do
    desktop-file-validate "$MIRA_APPS/$MIRA_ENTRY"
  done
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$MIRA_APPS"
fi
printf 'Open MiraOS, MiraOS — инструкция, or MiraOS — экспорт исходника from the applications menu.\n'
