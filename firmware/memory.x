/* Memory layout for STM32F411 (as example) */
/* Adjust these values for your specific MCU */

MEMORY
{
  /* Flash memory: 512KB */
  FLASH : ORIGIN = 0x08000000, LENGTH = 512K

  /* Main SRAM: 128KB */
  RAM : ORIGIN = 0x20000000, LENGTH = 120K

  /* Shared memory region: 8KB at end of SRAM */
  /* This region should be accessible by both MCU firmware and Linux (via debug interface or dual-port memory) */
  SHM : ORIGIN = 0x2001E000, LENGTH = 8K
}

/* Entry point */
ENTRY(Reset);

/* Stack pointer initialization */
_stack_start = ORIGIN(RAM) + LENGTH(RAM);

/* Shared memory symbols for firmware */
_shm_start = ORIGIN(SHM);
_shm_end = ORIGIN(SHM) + LENGTH(SHM);
_shm_size = LENGTH(SHM);

/* Optional: Place .bss in specific location */
SECTIONS
{
  /* Shared memory section */
  .shm (NOLOAD) :
  {
    . = ALIGN(64);
    *(.shm .shm.*)
  } > SHM
}
