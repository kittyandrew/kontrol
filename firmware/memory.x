/* Pico W: 2 MiB flash, 264 KiB SRAM (the two 4 KiB banks beyond 256K stay unused). The last 4 KiB flash
   sector is not program space: kontrol-flash writes the config there (CONFIG_ADDR in src/pico.rs). */
MEMORY {
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100 - 4K
    RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}

EXTERN(BOOT2_FIRMWARE)

SECTIONS {
    .boot2 ORIGIN(BOOT2) :
    {
        KEEP(*(.boot2));
    } > BOOT2
} INSERT BEFORE .text;
