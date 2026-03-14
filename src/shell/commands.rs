//! Shell commands

#![allow(dead_code)]

use core::ops::ControlFlow;

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
    println!("  ls    - List root directory files");
}

pub fn time() {
    println!("{} [ms]", timer::ticks_ms());
}

/// Lists files in current directory
pub fn ls() {
    crate::fs::fat16::with_volume(|vol| {
        let _ = vol
            .read_root_dir(|entry| {
                let name = entry.filename();
                println!("{}", name.as_str().unwrap_or("???"));
                ControlFlow::<()>::Continue(())
            })
            .expect("ls command failed");
    });
}

pub fn unknown(cmd: &str) {
    println!("Unknown command: {}", cmd);
}
