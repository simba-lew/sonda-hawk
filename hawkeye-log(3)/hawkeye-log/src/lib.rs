//! Balloon cutdown link between two boards. The role is a cargo feature and also picks the board
//! (see `board.rs`):
//!   `--features ground`:  Heltec V3; sends a ping every second and the ARM / FIRE / DISARM commands
//!                         typed in the serial monitor, logs telemetry
//!   `--features balloon`: LilyGO T-Beam S3; sends telemetry, takes pings and commands,
//!                         drives the burn pin, runs the failsafe cutdown
#![no_std]

#[cfg(not(any(feature = "ground", feature = "balloon")))]
compile_error!("choose a role: --features balloon | ground");

#[cfg(feature = "balloon")]
pub mod balloon;
pub mod board;
pub mod config;
pub mod cutdown;
#[cfg(feature = "balloon")]
pub mod failsafe;
#[cfg(feature = "ground")]
pub mod ground;
pub mod key;
#[cfg(feature = "balloon")]
pub mod logfile;
pub mod ping;
pub mod poke;
pub mod radio;
#[cfg(feature = "balloon")]
pub mod sensors;
pub mod state;
pub mod telemetry;
