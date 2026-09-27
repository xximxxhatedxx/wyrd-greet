fn main() {
    #[cfg(feature = "lock-mode")]
    println!("cargo:rustc-link-lib=pam");
}
