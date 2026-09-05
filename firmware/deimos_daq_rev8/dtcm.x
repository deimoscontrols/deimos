/*
 * Initialized read-only data that must be available through the Cortex-M7
 * data tightly coupled memory interface.
 *
 * cortex-m-rt deliberately sets __edata after user sections inserted after
 * .data. Keeping the same RAM and FLASH regions therefore makes its ordinary
 * .data startup copy initialize this section without a second copy routine.
 */
SECTIONS
{
  .dtcm : ALIGN(4)
  {
    KEEP(*(.dtcm .dtcm.*));
    . = ALIGN(4);
  } > RAM AT> FLASH
}
INSERT AFTER .data;

ASSERT(ADDR(.dtcm) == ADDR(.data) + SIZEOF(.data),
       "DTCM section is not contiguous with initialized data");
ASSERT(LOADADDR(.dtcm) == LOADADDR(.data) + SIZEOF(.data),
       "DTCM load image is not contiguous with initialized data");
ASSERT(__edata == ADDR(.dtcm) + SIZEOF(.dtcm),
       "DTCM section is outside cortex-m-rt data initialization boundaries");
