/* GreenLime OS — x86_64 Limine linker script.
 * Each LOAD section is 4K-aligned with uniform permissions.
 * Limine 12.x refuses PHDRs with different perms sharing a page.
 */
ENTRY(_start)
OUTPUT_FORMAT(elf64-x86-64)

PHDRS
{
    requests PT_LOAD FLAGS(4);
    text     PT_LOAD FLAGS(5);
    rodata   PT_LOAD FLAGS(4);
    data     PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0xffffffff80000000;

    .requests : {
        KEEP(*(.requests))
    } :requests

    . = ALIGN(0x1000);

    .text : {
        *(.text._start)
        *(.text*)
    } :text

    . = ALIGN(0x1000);

    .rodata : {
        *(.rodata*)
    } :rodata

    . = ALIGN(0x1000);

    .data : {
        *(.data*)
    } :data

    . = ALIGN(0x1000);

    .bss : {
        *(COMMON)
        *(.bss*)
    } :data

    /DISCARD/ : {
        *(.eh_frame*)
        *(.comment*)
        *(.note*)
        *(.note.gnu.property*)
    }
}
