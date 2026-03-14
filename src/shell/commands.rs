//! Shell commands

#![allow(dead_code)]

use crate::hal::ascii;
use crate::kernel::timer;
use crate::{print, println};

pub fn clear() {
    print!("{}", ascii::FF as char);
}

pub fn echo(args: &str) {
    println!("{args}");
}

pub fn help() {
    println!("Available commands:");
    println!("  clear - Clear the screen");
    println!("  echo  - Print arguments");
    println!("  help  - Show this help");
    println!("  time  - Show system ticks");
}

pub fn time() {
    println!("{} [ms]", timer::ticks_ms());
}

pub fn unknown(cmd: &str) {
    println!("Unknown command: {}", cmd);
}
