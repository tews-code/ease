# EASE OS - Rust Style Guide

Idiomatic Rust patterns used throughout the project.

---

## No `static mut` — Safe Alternatives

| Instead of... | Use... |
|---------------|--------|
| `static mut COUNTER: u64 = 0` | `static COUNTER: AtomicU64 = AtomicU64::new(0)` |
| `static mut DATA: Option<T> = None` | `static DATA: OnceCell<T> = OnceCell::new()` |
| `static mut SHARED: T = ...` | `static SHARED: Mutex<T> = Mutex::new(...)` |
| `static mut BUFFER: [u8; N] = ...` | `static BUFFER: Mutex<[u8; N]> = ...` |

## Interrupt-Safe Patterns

| Pattern | When to use |
|---------|-------------|
| `AtomicT` | Simple values accessed from interrupt handlers |
| `Mutex<T>` | Complex data, disable interrupts during access |
| `SpscQueue<T>` | Producer-consumer between interrupt and main code |
| `OnceCell<T>` | One-time initialization at startup |

## Ownership & Borrowing
- Proper lifetime annotations in drivers
- Zero-copy buffer handling where possible
- RAII for locks (guard patterns)

## Type System
- Newtype pattern for handles (`FileHandle(u32)`)
- Builder pattern for configuration
- Typestate pattern for peripheral initialization

## Error Handling
- Custom error types (manual impl, no thiserror)
- `Result<T, E>` everywhere, no panics in normal operation
- `?` operator for propagation

## Traits
- HAL traits for platform abstraction
- `Iterator` for directory listing
- `Read`/`Write` traits for I/O
- `Drop` for resource cleanup

## Generics & Const Generics
- `SpscQueue<T, const N: usize>`
- `HwSpinlock<const N: u32>`
- Generic drivers over SPI/I2C traits

## Unsafe
- Clearly documented unsafe blocks
- Minimal unsafe surface area
- Safe wrappers around hardware access
- **Never** `static mut`

## Modules & Visibility
- `pub(crate)` for internal APIs
- Clear module hierarchy
- Prelude modules for common imports
