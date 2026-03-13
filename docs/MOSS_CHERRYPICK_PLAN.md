# Plan: RP2350 Alignment & MOSS Cherry-Pick

## Context

EASE runs on QEMU virt with a single core and 128MB of unconstrained RAM — nothing like the RP2350's dual-core 520KB SRAM + 8MB PSRAM. This means code gets designed and reviewed against unrealistic constraints, leading to churn when real limits surface later. Additionally, the sister project MOSS has working infrastructure (PLIC, interrupt-driven UART, SpscRingBuf) that EASE needs for Phase 7+ but hasn't built yet.

This plan establishes a realistic QEMU environment and cherry-picks MOSS's best infrastructure into EASE's trait-based architecture. Each step is independently testable and committable.

## Dependency Graph

```
Step 1: QEMU + memory.x + core 1 parking
  |
  v
Step 2: board.rs (centralize constants)
  |
  +---> Step 3: SpscRingBuf
  |
  +---> Step 4: PLIC driver
          |
          v
        Step 5: External interrupt dispatch in trap handler
          |
          v
        Step 6: Interrupt-driven UART (needs Steps 3 + 5)
          |
          v
        Step 7: CriticalSection token
```

Steps 3 and 4 are independent of each other. Step 7 is independent but sequenced last.

---

## Step 1: QEMU Config, Memory Layout, Core 1 Parking

**Goal**: Dual-core QEMU with constrained memory regions matching RP2350 intent.

**Files**:
- `scripts/run.sh` — add `-smp 2` and `-m 16M` (headroom for framebuffer; constraints enforced by linker script)
- `memory-qemu.x` — restructure: define SRAM region (~520KB for code/stack/heap/BSS), separate VRAM region for framebuffer. Keep ASSERT guards. The addresses stay at 0x80000000 (QEMU virt requirement) but sizes reflect real constraints.
- `src/arch/boot.rs` — read `mhartid` CSR; if nonzero, jump to `wfi` parking loop. Only hart 0 proceeds with BSS zeroing and `main`.

**Design decisions**:
- RAM base stays at 0x80000000 (QEMU virt is fixed). Constraints enforced via linker regions, not address matching.
- Core 1 parks permanently in `wfi` loop. No stack allocated for it yet. Phase 12 will wake it.
- `-m 16M` rather than exactly 520KB+8MB because the framebuffer (1.2MB) and code sections need to fit. The linker script enforces the real constraints.

**Test**: `scripts/ci.sh` passes. Boot prints "Hello from EASE!" (hart 0 works). No crash (hart 1 parked). Optionally add a test printing `mhartid == 0`.

**MOSS reference**: MOSS has no core parking either — this is fresh work. Standard RISC-V SMP boot pattern.

---

## Step 2: board.rs (Centralize Constants)

**Goal**: Single source of truth for all hardware addresses and register offsets.

**Files**:
- New `src/board.rs` — organized sub-modules: `clint`, `plic`, `uart`, `framebuffer`, `sifive_test`, `virtio`. Forward-declare PLIC and UART register constants (IER, IIR, THR, RBR, LSR flags) even though they're not all used yet.
- `src/main.rs` — add `mod board;`
- `src/arch/timer.rs` — replace `CLINT_BASE`, `MTIME`, `MTIMECMP` with `board::clint::*`
- `src/drivers/uart.rs` — replace `UART_ADDRESS` and inline constants with `board::uart::*`
- `src/drivers/virtio/queue.rs` — replace `VIRTIO_BLK_PADDR` with `board::virtio::BASE`
- `src/drivers/ramfb.rs` — replace `FB_ADDR`, `WIDTH`, `HEIGHT` with `board::framebuffer::*`
- `src/qemu.rs` — replace `SIFIVE_TEST_ADDR` etc. with `board::sifive_test::*`

**Test**: Pure refactor — `scripts/ci.sh` passes, no behavioral change.

**MOSS reference**: `/home/thomas/src/moss/src/board.rs` — follow its structure closely.

---

## Step 3: SpscRingBuf

**Goal**: Lock-free single-producer single-consumer ring buffer for interrupt-safe communication.

**Files**:
- `src/kernel/collection.rs` — add `SpscRingBuf<T: Copy, const N: usize>` with `push(&self, T) -> Result<(), T>`, `pop(&self) -> Option<T>`, `is_full(&self) -> bool`. Add tests.

**Design decisions**:
- `Acquire` on loads, `Release` on stores (correct SPSC ordering — don't weaken to `Relaxed`)
- `UnsafeCell<MaybeUninit<T>>` array with atomic head/tail indices
- One sentinel slot always empty (capacity = N-1) to distinguish full from empty
- `&self` on push/pop (the whole point — no `&mut` needed)

**Test**: Unit tests covering push/pop, full buffer, FIFO order, wraparound, interleaved ops, edge cases (N=2 = one usable slot).

**MOSS reference**: `/home/thomas/src/moss/src/kernel/collection.rs` lines 270-327 and tests at lines 639-757.

---

## Step 4: PLIC Driver

**Goal**: Platform-Level Interrupt Controller interface for routing external interrupts.

**Files**:
- New `src/drivers/plic.rs` — five functions: `set_priority(source, priority)`, `set_threshold(threshold)`, `enable(source)`, `claim() -> u32`, `complete(source)`. All use `board::plic::*` constants and volatile MMIO.
- `src/drivers/mod.rs` — add `pub mod plic;`

**Design decisions**:
- Context 0 only (M-mode, hart 0). No multi-context abstraction needed yet.
- No `init()` function — each device calls `set_priority`/`enable` for its own IRQ during its own init.
- Don't enable MEIE in mie CSR yet (that's step 5).

**Test**: Compiles without error. Real testing happens in step 5/6 when interrupts flow end-to-end.

**MOSS reference**: `/home/thomas/src/moss/src/drivers/plic.rs` — ~35 lines, copy the structure.

---

## Step 5: External Interrupt Dispatch in Trap Handler

**Goal**: Wire up mcause code 11 (external interrupt) to PLIC claim/complete and device-specific handlers.

**Files**:
- `src/arch/trap.rs` — add `INTERRUPT_EXTERNAL = 11` arm. Pattern: `claim()` -> match IRQ -> handle -> `complete(irq)`. For UART0_IRQ: stub handler (drain RBR to clear interrupt, or just log). For VIRTIO0_IRQ: acknowledge and ignore. For IRQ 0: spurious, skip complete.
- `src/main.rs` (`kernel_init`) — after driver init, call `plic::set_priority` and `plic::enable` for UART0 and VIRTIO0 IRQs. Enable MEIE (bit 11) in `mie` CSR. Call `uart::enable_uart_interrupts()` (sets IER bit 0 on the UART).

**Design decisions**:
- UART IIR dispatch goes in the trap handler (pragmatic, matches MOSS). Read IIR to determine RX vs TX vs line status, dispatch accordingly. Step 6 fills in the real handlers.
- Stub handler for UART RX: read and discard bytes from RBR to clear the interrupt. This prevents an infinite interrupt loop while keeping step 5 self-contained.

**Test**: `scripts/ci.sh` passes. No spurious interrupt panics. Typing in QEMU console doesn't crash (bytes are drained by stub handler).

**MOSS reference**: `/home/thomas/src/moss/src/arch/trap.rs` lines 111-137 for the external interrupt dispatch pattern.

---

## Step 6: Interrupt-Driven UART

**Goal**: Replace polled UART with ring-buffered interrupt-driven I/O.

**Files**:
- `src/drivers/uart.rs` — major rework:
  - Add `static RX_BUF: SpscRingBuf<u8, 64>` and `static TX_BUF: SpscRingBuf<u8, 256>`
  - Add `handle_uart_rx_interrupt()`: drain RBR into RX_BUF while LSR shows data ready
  - Add `handle_uart_tx_interrupt()`: pop TX_BUF, write to THR. If empty, disable THRE interrupt.
  - `UartReader::read_byte()` -> pop from RX_BUF (keeps `hal::Reader` trait)
  - `UartWriter::write_byte()` -> push to TX_BUF, enable THRE interrupt (keeps `hal::Writer` trait)
  - Add `direct_write_byte()` for panic handler / interrupt-context debug prints
- `src/arch/trap.rs` — replace stub UART handler with calls to `handle_uart_rx_interrupt()` / `handle_uart_tx_interrupt()` based on IIR value
- `src/io.rs` — `printd!`/`printdln!` must use `direct_write_byte()`, not the buffered path (SPSC single-producer invariant: only main thread writes TX_BUF)

**Design decisions**:
- TX buffering is optional for this step. RX interrupts alone are the big win (no lost keystrokes). TX can stay polled if simpler. Decision point for the user.
- SPSC invariant: RX_BUF producer = interrupt handler, consumer = main thread. TX_BUF producer = main thread, consumer = interrupt handler. `printd!` must bypass TX_BUF.
- IIR values: 0b0100/0b1100 = RX, 0b0010 = TX (THRE), 0b0110 = line status (read LSR to clear)

**Test**: `scripts/ci.sh` passes. Shell still works (type commands, get responses). Boot messages print correctly. Hold a key during VirtIO I/O — keystrokes should not be lost.

**MOSS reference**: `/home/thomas/src/moss/src/drivers/uart.rs` — the full interrupt-driven pattern with SpscRingBuf.

---

## Step 7: CriticalSection Token

**Goal**: Zero-cost compile-time proof that interrupts are disabled.

**Files**:
- `src/kernel/sync.rs` — add `CriticalSection<'cs>` (zero-sized, `PhantomData<&'cs ()>`, private constructor) and `with_interrupts_disabled<F, R>(f: F) -> R where F: FnOnce(CriticalSection<'_>) -> R`.

**Design decisions**:
- Complements SpinLock, does not replace it. SpinLock = multi-core mutual exclusion. CriticalSection = compile-time proof of interrupt-disabled state.
- Do NOT import MOSS's `StaticMutex` — it uses `Cell<bool>`, which is single-core only. EASE's SpinLock is already correct for multi-core.
- No need to retrofit into existing code immediately. Establish the pattern, use in one or two places.

**Test**: Test that `with_interrupts_disabled` works (`|_cs| 42` returns 42). The real value is compile-time — the token can't escape the closure.

**MOSS reference**: `/home/thomas/src/moss/src/kernel/sync.rs` lines 14-41.

---

## Verification

After all 7 steps:
1. `scripts/ci.sh` passes
2. `qemu-system-riscv32` runs with `-smp 2` — hart 0 boots, hart 1 parked
3. Shell is fully functional with interrupt-driven keyboard input
4. No magic numbers outside `board.rs`
5. Memory regions in linker script reflect realistic constraints
6. `SpscRingBuf` and `CriticalSection` available for Phase 7+ development
