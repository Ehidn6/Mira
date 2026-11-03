/* Minimal Multiboot v1 -> long-mode handoff. All kernel behavior is Mira code.
 * Linked at 0x100000. Kernel payload is at 0x101000. See rebuild.sh.
 * Reserved: tables 0x200000..0x203000, boot-info slot 0x203000,
 * scratch 0x210000..0x300000, stack 0x300000..0x400000.
 */
.section .text
.code32
.global _start
.long 0x1badb002, 3, -(0x1badb002 + 3)
_start:
    cli
    cld
    cmpl $0x2badb002, %eax
    jne stopped
    movl %ebx, %esi
    /* Establish our data selectors before touching RAM; incoming GDT need not survive. */
    lgdt gdt_pointer
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movl $0x400000, %esp
    movl $0x200000, %edi
    xorl %eax, %eax
    movl $(0x3000 / 4), %ecx
    rep stosl
    movl %esi, 0x203000
    movl $0, 0x203004
    movl $0x201003, 0x200000
    movl $0x202003, 0x201000
    movl $0x202000, %edi
    movl $0x83, %eax
    movl $512, %ecx
map_page:
    movl %eax, (%edi)
    addl $0x200000, %eax
    addl $8, %edi
    loop map_page
    movl %cr4, %eax
    orl $0x20, %eax
    movl %eax, %cr4
    movl $0x200000, %eax
    movl %eax, %cr3
    movl $0xc0000080, %ecx
    rdmsr
    orl $0x100, %eax
    wrmsr
    movl %cr0, %eax
    orl $0x80000001, %eax
    movl %eax, %cr0
    ljmp $0x08, $long_entry
stopped:
    cli
    hlt
    jmp stopped
.balign 8
gdt:
    .quad 0
    .quad 0x00af9a000000ffff
    .quad 0x00cf92000000ffff
gdt_pointer:
    .word . - gdt - 1
    .long gdt
.code64
long_entry:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    xorw %ax, %ax
    movw %ax, %fs
    movw %ax, %gs
    movl $0x400000, %esp
    xorq %rbp, %rbp
    movl $0x101000, %eax
    jmp *%rax
