//! TunnelHop privileged service. Windows only; the real entry point lands in step 10.

#[cfg(windows)]
fn main() {
    eprintln!("th-service: not implemented yet");
    std::process::exit(1);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("th-service runs on Windows only");
    std::process::exit(1);
}
