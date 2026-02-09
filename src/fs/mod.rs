//! Filesystem for EASE

#![allow(dead_code)]

mod fat16;

#[derive(Debug)]
pub enum FsError {
    DeviceError,
    NotFat16,
}
