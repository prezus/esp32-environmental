fn main() {
    println!("cargo:rerun-if-changed=sdkconfig.defaults");
    println!("cargo:rerun-if-changed=partitions.csv");
    println!("cargo:rerun-if-changed=cfg.toml");
    embuild::espidf::sysenv::output();
}
