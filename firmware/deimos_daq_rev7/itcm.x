SECTIONS
{
  .itcm : ALIGN(4)
  {
    __sitcm = .;
    KEEP(*(.itcm .itcm.*));
    . = ALIGN(4);
    __eitcm = .;
  } > ITCM AT> FLASH

  __siitcm = LOADADDR(.itcm);
}
INSERT BEFORE .data;

ASSERT(SIZEOF(.itcm) <= LENGTH(ITCM),
       "ITCM section exceeds available ITCM");
ASSERT(__edata == ADDR(.data) + SIZEOF(.data),
       "ITCM section disrupted cortex-m-rt data boundaries");
