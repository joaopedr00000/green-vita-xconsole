/* Leave space for the SCE module/import tables that vita-elf-create
 * appends to the first PT_LOAD. No output section or padding bytes are
 * emitted. Use an augmenting script rather than relying on the newer
 * VitaSDK __sce_headroom symbol. Keep a full 16 KiB safety margin.
 */
SECTIONS
{
  . = (ALIGN(0x10000) - . < 0x4000) ? ALIGN(0x10000) + 0x10 : .;
}
INSERT AFTER .eh_frame;
