# Refactoring Plan: Ownership-Passing Framebuffer

Addresses CRITICAL2.md point 8 (Console Framebuffer Ownership Ceremony).

Replace the `DISPLAY` global as Console owner with Rust's move semantics —
the FrameBuffer is the root resource returned from init, and Console is a
temporary wrapper around it. Each step compiles and passes `scripts/ci.sh`
before moving to the next.

**Key design insight:** The FrameBuffer has the longest lifetime (boot to
shutdown). Console is one *mode* of using it (text rendering), and apps like
Doom are another (raw pixel access). The outermost owner should be the
longest-lived resource — wrappers come and go around it.

```
let fb = kernel_init();
// splash_screen(&mut fb);         // future: draw logo
let console = Console::new(fb);    // text mode
// ... shell runs ...
let fb = console.into_fb();        // back to raw framebuffer
doom_run(&mut fb);                 // app mode
let console = Console::new(fb);    // text mode again (fresh state)
```

## Step 1: Make `kernel_init` return the FrameBuffer

Currently `kernel_init()` creates a FrameBuffer, wraps it in a Console,
stuffs the Console into `DISPLAY`, and returns nothing. Then `main()`
immediately pulls the Console back out with `take_console()`.

**Change:** Have `kernel_init()` return the `FrameBuffer` directly. In
`main()`, wrap it in a Console as a separate step.

The `println!("Hello from EASE!")` on line 107 currently goes through
`DISPLAY` (which has the Console at that point). After this change, `DISPLAY`
never holds a Console in production, so replace that `println!` with a direct
`write!(console, ...)` + `printdln!(...)`.

**Files to touch:**
- `main.rs:kernel_init` — stop creating Console and calling
  `DISPLAY.lock().init(console)`, return `FrameBuffer` instead
- `main.rs:main` (the `#[cfg(not(test))]` one) — receive the FrameBuffer,
  wrap it in a Console, write the boot message directly, pass Console to Shell

After this step, `DISPLAY` is still used by tests and by `print!`, but
production code no longer puts a Console into it.

## Step 2: Make `print!` UART-only

Currently `print!` locks `DISPLAY` and calls `write!` on it. Since production
code no longer stores a Console in `DISPLAY`, the framebuffer write is already
a no-op (it's `Headless`). This step makes that explicit.

**Change:** Make `print!` use `DirectWriter` (UART-only), same as `printd!`.

At this point `print!` and `printd!` become identical. You could either merge
them (make `print!` just call `printd!`) or keep both as aliases for now.

**Files to touch:**
- `io.rs:print!` macro — replace `DISPLAY.lock()` write with `DirectWriter`
  write

Production code is now fully decoupled from `DISPLAY`. Shell output goes
through the owned Console. `print!`/`println!` go to UART.

## Step 3: Check virtio init message

`src/drivers/virtio/mod.rs:141` uses `println!` to print `"virtio-blk:
capacity is ... bytes"`. After Step 2, this only goes to UART. That's fine for
a boot diagnostic — it prints during `kernel_init`, before the Console exists
anyway.

**Decision:** Leave as-is. Boot diagnostics on UART only is normal for an OS.

No code change needed.

## Step 4: Update tests

The test code uses `DISPLAY` heavily — benchmarks lock it, call `put_char`,
`release_to_app`, etc. Tests need a Console to benchmark against.

**Change:** In the test `main()` (`#[cfg(test)]`), create a FrameBuffer and
Console, put the Console in `DISPLAY` for the benchmarks to use, the same way
`kernel_init` does today.

**Files to touch:**
- `main.rs` — test `main()` calls `kernel_init()` which no longer inits
  DISPLAY. Add a framebuffer/console init for test mode specifically.
  Alternatively, have `kernel_init` still init DISPLAY only in `#[cfg(test)]`.

The test `test_printdln_while_display_locked` tests that `printdln!` works
while `DISPLAY` is locked. After step 2, `print!` no longer locks `DISPLAY`,
so this test is testing a scenario that can't happen in production anymore.
Remove it or keep it as a sanity check for the test benchmarks.

## Step 5: Clean up `DisplayManager`

After steps 1-4, production code never uses `DisplayManager` for Console
ownership. The only users are test benchmarks.

**Change:** Remove `take_console()` from `DisplayManager` — it's no longer
called. Consider whether `release_to_app` / `return_to_console` are still
needed (only for test benchmarks). If you want to keep benchmarks working
through `DISPLAY`, leave them. Otherwise, refactor benchmarks to own a Console
directly.

**Files to touch:**
- `drivers/mod.rs` — remove dead methods

## Step 6: Add `Console::into_fb()` method

Add a method that consumes the Console and returns the FrameBuffer. This
enables clean mode transitions (e.g. shell → Doom → shell) without needing
`take()` or `Option` wrappers:

```rust
let fb = console.into_fb();   // Console is gone, fb is back
doom_run(&mut fb);
let console = Console::new(fb);  // fresh Console, clean screen
```

Returning to the shell creates a fresh Console (no preserved scroll state).
This is the "games console" model — mode switches reset the display.

**Files to touch:**
- `shell/console.rs` — add `into_fb(self) -> FrameBuffer`

## Step 7 (optional): Remove `Headless` variant

If after step 5 the only `DisplayMode` variants used are `Console` (in tests)
and `Headless` (as initial state), consider whether `DisplayManager` is still
earning its keep or whether tests should just own a `Console` directly. This is
a larger cleanup you can defer.

## Summary

| Step | What                              | Risk   |
|------|-----------------------------------|--------|
| 1    | `kernel_init` returns FrameBuffer | Low    |
| 2    | `print!` becomes UART-only        | Low    |
| 3    | Check virtio println              | None   |
| 4    | Fix test init                     | Medium |
| 5    | Remove dead DisplayManager methods| Low    |
| 6    | Add `Console::into_fb()`          | Low    |
| 7    | Optional: simplify further        | Low    |

Steps 1 and 2 are the core of the refactor. Steps 3-5 are cleanup. Step 6
enables future mode transitions (Doom, splash screen). After step 2, the
ownership-passing approach is in place and `Headless` can never cause silent
output loss in production.
