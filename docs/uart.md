# UART on QEMU virt

This document describes UART (Universal Asynchronous Receiver/Transmitter) serial communication as implemented in EASE for the QEMU virt machine.

## UART Protocol Basics

UART is a simple serial protocol for transmitting bytes between devices:

| Parameter | Description |
|-----------|-------------|
| **Baud rate** | Speed in bits/second (e.g., 115200) |
| **Data bits** | Bits per character (typically 8) |
| **Stop bits** | End-of-byte marker (typically 1) |
| **Parity** | Error detection (none/even/odd) |

A typical configuration is **8N1**: 8 data bits, no parity, 1 stop bit.

### Transmission Format

```
    ┌─────┬─────────────────────────────────┬──────┐
    │Start│    8 Data Bits (LSB first)      │ Stop │
    │ bit │ D0  D1  D2  D3  D4  D5  D6  D7  │ bit  │
    └─────┴─────────────────────────────────┴──────┘

    Idle line is HIGH (1)
    Start bit is LOW (0)
    Stop bit is HIGH (1)
```

### Flow Control

Flow control prevents data loss when the receiver can't keep up:

- **None**: No flow control (simplest, used by QEMU)
- **Hardware (RTS/CTS)**: Extra signal lines pause transmission
- **Software (XON/XOFF)**: Special bytes pause/resume transmission

QEMU's virtual UART has infinite buffers, so flow control isn't needed.

## QEMU virt 16550 UART

QEMU's `virt` machine emulates a **16550 UART** at address `0x10000000`.

### Memory Map

| Offset | Register | Read | Write |
|--------|----------|------|-------|
| +0 | RBR/THR | Receive Buffer | Transmit Holding |
| +1 | IER | Interrupt Enable | Interrupt Enable |
| +2 | IIR/FCR | Interrupt ID | FIFO Control |
| +3 | LCR | Line Control | Line Control |
| +4 | MCR | Modem Control | Modem Control |
| +5 | LSR | Line Status | — |
| +6 | MSR | Modem Status | — |
| +7 | SCR | Scratch | Scratch |

### Key Registers for EASE

**THR (Transmit Holding Register) - offset +0, write-only**

Write a byte here to transmit it. QEMU processes it immediately.

```
Bit 7-0: Data byte to transmit
```

**RBR (Receive Buffer Register) - offset +0, read-only**

Read received bytes from here. Only valid when LSR bit 0 is set.

```
Bit 7-0: Received data byte
```

**LSR (Line Status Register) - offset +5, read-only**

Check transmission and reception status.

```
Bit 0: Data Ready (DR) - 1 = byte available in RBR
Bit 1: Overrun Error (OE)
Bit 2: Parity Error (PE)
Bit 3: Framing Error (FE)
Bit 4: Break Interrupt (BI)
Bit 5: THR Empty (THRE) - 1 = ready to transmit
Bit 6: Transmitter Empty (TEMT)
Bit 7: Error in FIFO
```

### Simplified Usage

For QEMU (no real timing constraints):

**Transmit:** Just write to THR. QEMU handles it instantly.

```rust
const UART_BASE: usize = 0x10000000;
unsafe { core::ptr::write_volatile(UART_BASE as *mut u8, byte); }
```

**Receive:** Check LSR bit 0, then read RBR.

```rust
const UART_LSR: usize = 0x10000005;
const UART_RBR: usize = 0x10000000;

fn read_byte() -> Option<u8> {
    unsafe {
        let lsr = core::ptr::read_volatile(UART_LSR as *const u8);
        if lsr & 1 != 0 {
            Some(core::ptr::read_volatile(UART_RBR as *const u8))
        } else {
            None
        }
    }
}
```

## EASE I/O Abstraction

EASE abstracts UART access through the `Writer` trait in `src/io.rs`.

### Writer Trait

```rust
/// Trait for byte-level output
pub trait Writer {
    /// Write a single byte
    fn write_byte(&mut self, byte: u8);

    /// Write a string as bytes (default implementation)
    fn write_str(&mut self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}
```

### UartWriter

The `UartWriter` struct implements `Writer` for QEMU's UART:

```rust
const UART_ADDRESS: usize = 0x10000000;

pub struct UartWriter;

impl Writer for UartWriter {
    fn write_byte(&mut self, byte: u8) {
        unsafe {
            write_volatile(UART_ADDRESS as *mut u8, byte);
        }

        #[cfg(test)]
        test_io::capture(byte);  // Also capture for tests
    }
}
```

It also implements `core::fmt::Write` for use with `write!` macro:

```rust
impl core::fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        Writer::write_str(self, s);
        Ok(())
    }
}
```

### print! and println! Macros

These macros provide formatted output to UART:

```rust
/// Print to UART without newline
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::io::UartWriter, $($arg)*);
    }}
}

/// Print to UART with newline
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::print!("{}\n", format_args!($($arg)*)) };
}
```

### Usage Examples

```rust
// Simple string
println!("Hello from EASE!");

// Formatted output
let count = 42;
println!("Count: {}", count);

// Multiple values
println!("x={}, y={}, sum={}", 10, 20, 10 + 20);

// Without newline
print!("Loading");
print!("...");
println!(" done!");

// Debug formatting
let arr = [1, 2, 3];
println!("Array: {:?}", arr);

// Hex formatting
println!("Address: {:#x}", 0x10000000);
```

## Test Output Capture

In test builds, all UART output is also captured to a buffer for verification.

### API

```rust
// In src/io.rs, only available in #[cfg(test)]
pub mod test_io {
    /// Clear the capture buffer (call before test)
    pub fn clear();

    /// Get captured output as string
    pub fn output() -> &'static str;

    /// Check if output contains substring
    pub fn contains(expected: &str) -> bool;

    /// Check if output equals expected exactly
    pub fn equals(expected: &str) -> bool;
}
```

### Example Test

```rust
#[test_case]
fn test_println_output() {
    test_io::clear();
    println!("hello");
    assert!(test_io::equals("hello\n"));
}

#[test_case]
fn test_formatted_output() {
    test_io::clear();
    println!("value: {}", 42);
    assert!(test_io::contains("42"));
}
```

### Implementation Notes

- Buffer size: 4KB (sufficient for most tests)
- Uses `UnsafeCell` + `AtomicUsize` for thread-safe length tracking
- Assumes single-threaded test execution
- Assumes all output is valid UTF-8 (true for print!/println!)

## Future: Real Hardware

On the RP2350, UART will use different registers and require:

- Proper baud rate configuration
- Pin multiplexing (GPIO function select)
- Possibly interrupt-driven RX for keyboard input

The `Writer` trait allows swapping implementations without changing code that uses `print!`/`println!`.

## References

- [16550 UART Wikipedia](https://en.wikipedia.org/wiki/16550_UART)
- [QEMU virt machine source](https://github.com/qemu/qemu/blob/master/hw/riscv/virt.c)
- [OSDev UART article](https://wiki.osdev.org/Serial_Ports)
