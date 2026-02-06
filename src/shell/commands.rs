//! Shell commands

use core::fmt::Write;

use crate::arch::timer;
use crate::drivers::console::Console;

pub fn clear(console: &mut Console) {
    console.clear();
}

pub fn echo(console: &mut Console, args: &str) {
    let _ = writeln!(console, "{}", args);
}

pub fn help(console: &mut Console) {
    use core::fmt::Write;
    let _ = writeln!(console, "Available commands:");
    let _ = writeln!(console, "  clear - Clear the screen");
    let _ = writeln!(console, "  echo  - Print arguments");
    let _ = writeln!(console, "  help  - Show this help");
    let _ = writeln!(console, "  time  - Show system ticks");
}

pub fn time(console: &mut Console) {
    let time = timer::ticks_ms();
    use core::fmt::Write;
    let _ = writeln!(console, "{} [ms]", time);
}

pub fn unknown(console: &mut Console, cmd: &str) {
    let _ = writeln!(console, "Unknown command: {}", cmd);
}
