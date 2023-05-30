build:
	mkdir -p debian/tmp_files/.cargo
	CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse CARGO_HOME=debian/tmp_files/.cargo cargo build --release

clean:
	cargo clean
