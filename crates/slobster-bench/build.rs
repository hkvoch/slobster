use rustc_version::version;

fn main() {
	println!("cargo::warning=VERSION {}", version().unwrap());
}
