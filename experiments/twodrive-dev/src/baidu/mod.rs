//! Baidu Netdisk Open Platform adapter. No cookies or third-party relay.
pub mod auth;
mod drive;
mod http;
pub use drive::BaiduDrive;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod write_tests;
