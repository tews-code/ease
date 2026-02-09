//! QEMU RAM Frame Buffer driver
//!
//! Configures QEMU's ramfb device to display a framebuffer.
//! The framebuffer is a region of RAM that QEMU reads and displays.

pub struct FrameBuffer;

impl FrameBuffer {
    // Screen settings
    const WIDTH: usize = 640;
    const HEIGHT: usize = 480;
    const STRIDE: usize = Self::WIDTH * 4; // 4 bytes per pixel

    // Framebuffer address (after stack at 0x80100000)
    const FB_ADDR: usize = 0x80200000;

    // Initialize the framebuffer
    pub fn init() -> Self {
        // Configuration sent to QEMU (all fields big-endian)
        #[repr(C, packed)]
        struct RamfbConfig {
            addr: u64,
            fourcc: u32,
            flags: u32,
            width: u32,
            height: u32,
            stride: u32,
        }

        let config = RamfbConfig {
            addr: (Self::FB_ADDR as u64).to_be(),
            fourcc: Colour::pixel_format().to_be(),
            flags: 0,
            width: (Self::WIDTH as u32).to_be(),
            height: (Self::HEIGHT as u32).to_be(),
            stride: (Self::STRIDE as u32).to_be(),
        };

        // Packed struct must be 28 bytes
        const _: () = assert!(core::mem::size_of::<RamfbConfig>() == 28);

        write_fw_cfg_dma(&config);
        crate::println!(
            "ramfb: {}x{} at {:#x}",
            Self::WIDTH,
            Self::HEIGHT,
            Self::FB_ADDR
        );

        // Send RamfbConfig to QEMU via DMA
        fn write_fw_cfg_dma(config: &RamfbConfig) {
            // fw_cfg MMIO addresses (QEMU virt machine)
            const FW_CFG_DMA: usize = 0x10100010;
            // ramfb selector (found by enumerating fw_cfg directory)
            // This value may change with different QEMU versions
            const RAMFB_SELECTOR: u16 = 0x25;
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
                core::ptr::write_volatile(
                    FW_CFG_DMA as *mut u32,
                    ((dma_addr >> 32) as u32).to_be(),
                );
                core::ptr::write_volatile((FW_CFG_DMA + 4) as *mut u32, (dma_addr as u32).to_be());

                // Wait for completion (QEMU sets control to 0)
                while core::ptr::read_volatile(&dma.control as *const u32) != 0 {
                    core::hint::spin_loop();
                }
            }
        }

        Self // Return FrameBuffer on init to be held by device using RAM fb
    }

    // Helper function to create a slice over the framebuffer
    fn buffer(&mut self) -> &mut [u32] {
        unsafe {
            // Safety: the framebuffer has been created and is safe for writes
            core::slice::from_raw_parts_mut(Self::FB_ADDR as *mut u32, self.width() * self.height())
        }
    }

    /// Set a pixel at (x, y) to colour
    #[expect(dead_code)]
    pub fn set_pixel(&mut self, x: usize, y: usize, colour: Colour) {
        if x < self.width() && y < self.height() {
            let w = self.width();
            self.buffer()[y * w + x] = colour.as_raw();
        }
    }

    /// Set a row of pixels at (x, y) to colours provided in slice
    pub fn set_pixels(&mut self, x: usize, y: usize, colours: &[u32]) {
        if x + colours.len() < self.width() && y < self.height() {
            let w = self.width();
            self.buffer()[y * w + x..y * w + x + colours.len()].copy_from_slice(colours);
        }
    }

    /// Set a row of pixels at `y` to colour
    #[expect(dead_code)]
    pub fn set_row(&mut self, y: usize, colour: Colour) {
        if y < self.height() {
            let w = self.width();
            self.buffer()[y * w..(y + 1) * w].fill(colour.as_raw());
        }
    }

    /// Scroll the frame buffer by `scroll_height` pixels
    pub fn scroll(&mut self, scroll_height: usize, bg: Colour) {
        let w = self.width();
        self.buffer().copy_within(scroll_height * w.., 0);
        self.fill_rows(self.height() - scroll_height, scroll_height, bg);
    }

    /// Fills rows with `colour` starting at `y` with 'height` pixels
    pub fn fill_rows(&mut self, start_y: usize, height: usize, colour: Colour) {
        if start_y + height <= self.height() {
            let w = self.width();
            self.buffer()[start_y * w..(start_y + height) * w].fill(colour.as_raw());
        }
    }

    /// Fill frame buffer with colour
    pub fn fill(&mut self, colour: Colour) {
        self.buffer().fill(colour.as_raw());
    }

    /// Get framebuffer width in pixels
    pub fn width(&self) -> usize {
        Self::WIDTH
    }

    /// Get framebuffer height in pixels
    pub fn height(&self) -> usize {
        Self::HEIGHT
    }
}

/// Colour definition for frame buffer
#[derive(Clone, Copy)]
pub struct Colour(u32);

#[expect(dead_code)]
impl Colour {
    // Pixel format: XR24 = 0x00RRGGBB (32-bit, X ignored)
    const FOURCC_XR24: u32 = 0x34325258;

    // Predefined colors
    pub const BLACK: Self = Self(0x000000);
    pub const WHITE: Self = Self(0xFFFFFF);
    pub const RED: Self = Self(0xFF0000);
    pub const GREEN: Self = Self(0x00FF00);
    pub const BLUE: Self = Self(0x0000FF);

    #[expect(dead_code)]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    pub const fn as_raw(&self) -> u32 {
        self.0
    }

    pub fn pixel_format() -> u32 {
        Self::FOURCC_XR24
    }
}
