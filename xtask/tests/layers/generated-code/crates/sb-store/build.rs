fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{out}/generated.rs"), "pub struct Generated;\n").unwrap();
}
