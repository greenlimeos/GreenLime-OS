/* greenlime-init — user ELF, loaded at 0x400000 by the kernel.
 * Same 4K discipline as the kernel: Limine-style loaders aren't involved
 * here, but our own mapper assigns one permission set per page.
 */
ENTRY(_start)
OUTPUT_FORMAT(elf64-x86-64)

PHDRS
{
    text   PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0x400000;

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
