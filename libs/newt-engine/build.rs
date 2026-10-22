fn main() {
    println!(
        "cargo:rustc-env=NEWT_TARGET_TRIPLE={}",
        std::env::var("TARGET").unwrap()
    );
}
