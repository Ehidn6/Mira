#!/bin/sh
# Developer-only rebuilding of the UEFI adapter; normal Cargo builds use this blob.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
clang --target=x86_64-pc-win32-coff -c loader.c -o "$tmp/loader.obj" -O2 \
  -ffreestanding -fno-builtin -fno-stack-protector -fshort-wchar -mno-red-zone \
  -mno-sse -mno-mmx -Wall -Wextra -Wno-unused-parameter
lld-link /subsystem:efi_application /entry:efi_main /nodefaultlib /machine:x64 \
  /dynamicbase /nxcompat /out:BOOTX64.EFI "$tmp/loader.obj"
chmod 644 BOOTX64.EFI
