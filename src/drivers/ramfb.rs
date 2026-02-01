//! QEMU RAM Frame Buffer driver
//!
//! Configures QEMU's ramfb device to display a framebuffer.
//! The framebuffer is a region of RAM that QEMU reads and displays.

use core::ptr::{read_volatile, write_volatile};

// fw_cfg MMIO addresses (QEMU virt machine)
const FW_CFG_DMA: usize = 0x10100010;

// ramfb selector (found by enumerating fw_cfg directory)
// This value may change with different QEMU versions
const RAMFB_SELECTOR: u16 = 0x25;

// Framebuffer settings
const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
const STRIDE: u32 = WIDTH * 4; // 4 bytes per pixel

// Pixel format: XR24 = 0x00RRGGBB (32-bit, X ignored)
const FOURCC_XR24: u32 = 0x34325258;

// Framebuffer address (after stack at 0x80100000)
const FB_ADDR: usize = 0x80200000;

/// Configuration sent to QEMU (all fields big-endian)
#[repr(C, packed)]
struct RamfbConfig {
    addr: u64,
    fourcc: u32,
    flags: u32,
    width: u32,
    height: u32,
    stride: u32,
}

const _: () = assert!(core::mem::size_of::<RamfbConfig>() == 28);

/// Initialize the framebuffer
pub fn init() {
    let config = RamfbConfig {
        addr: (FB_ADDR as u64).to_be(),
        fourcc: FOURCC_XR24.to_be(),
        flags: 0,
        width: WIDTH.to_be(),
        height: HEIGHT.to_be(),
        stride: STRIDE.to_be(),
    };

    write_fw_cfg_dma(&config);
    crate::println!("ramfb: {}x{} at {:#x}", WIDTH, HEIGHT, FB_ADDR);
}

/// Send RamfbConfig to QEMU via DMA
fn write_fw_cfg_dma(config: &RamfbConfig) {
    const SELECT: u32 = 0x08;
    const WRITE: u32 = 0x10;

    // DMA descriptor (on stack)
    #[repr(C, align(16))]
    struct DmaAccess {
        control: u32,
        length: u32,
        address: u64,
    }

    let mut dma = DmaAccess {
        control: (SELECT | WRITE | ((RAMFB_SELECTOR as u32) << 16)).to_be(),
        length: (core::mem::size_of::<RamfbConfig>() as u32).to_be(),
        address: (config as *const RamfbConfig as u64).to_be(),
    };

    unsafe {
        // Memory fence before triggering DMA
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Write DMA descriptor address to fw_cfg (triggers transfer)
        let dma_addr = &mut dma as *mut DmaAccess as u64;
        write_volatile(FW_CFG_DMA as *mut u32, ((dma_addr >> 32) as u32).to_be());
        write_volatile((FW_CFG_DMA + 4) as *mut u32, (dma_addr as u32).to_be());

        // Wait for completion (QEMU sets control to 0)
        while read_volatile(&dma.control as *const u32) != 0 {
            core::hint::spin_loop();
        }
    }
}

/// Clear screen to a colour (0xRRGGBB)
pub fn clear(colour: u32) {
    unsafe {
        let fb = FB_ADDR as *mut u32;
        for i in 0..((WIDTH * HEIGHT) as usize) {
            write_volatile(fb.add(i), colour);
        }
    }
}

/// Set a pixel at (x, y) to colour (0xRRGGBB)
#[allow(dead_code)]
pub fn set_pixel(x: u32, y: u32, colour: u32) {
    if x < WIDTH && y < HEIGHT {
        let offset = (y * WIDTH + x) as usize;
        unsafe {
            write_volatile((FB_ADDR as *mut u32).add(offset), colour);
        }
    }
}

/// Get framebuffer dimensions
#[allow(dead_code)]
pub fn width() -> u32 {
    WIDTH
}

#[allow(dead_code)]
pub fn height() -> u32 {
    HEIGHT
}
