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
ASSERT(__sdata == ADDR(.data),
       "ITCM section disrupted the cortex-m-rt data start boundary");
