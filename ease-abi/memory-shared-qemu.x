/*
 * Common memory layout for user apps and EASE
 *
 * User programs are restricted to NAPOT regions to comply with RP2350 NAPOT-only PMP.
 *
 * The user heap is at the start of PD0
 *
 * We limit all user programs to fit into 4KB .text for now.
 * The .user_text region is followed by .user_data and then .user_bss, also limited to 2KB each.
 * Currently user programs are loaded at the end of the first half of PSRAM (0x81400000)
 */

__user_text_origin = 0x81400000;
__user_text_size = 4K;

__user_data_origin = __user_text_origin + __user_text_size;
__user_data_size = 2K;
__user_bss_origin = __user_data_origin + __user_data_size;
__user_bss_size = 2K;

__user_heap_origin = 0x80000000;
__user_heap_sram_size = 256K;
__user_heap_psram_size = 4M;
