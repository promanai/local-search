#[cfg(windows)]
fn main() {
    if let Err(error) = localsearch_desktop::run() {
        eprintln!("LocalSearch Desktop failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LocalSearch Desktop v0.1 is currently available on Windows");
    std::process::exit(2);
}
