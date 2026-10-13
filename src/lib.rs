//! tvdocker — multi-window Docker TUI built on Turbo Vision (tvision-rs).
#![forbid(unsafe_code)]

pub mod app;
pub mod bridge;
pub mod cli;
pub mod core;
pub mod domain;
#[cfg(test)]
mod testutil;
pub mod views;
pub mod widgets;
