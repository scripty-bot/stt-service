fn main() {
	match pkg_config::probe_library("openvino ") {
		Ok(_) => println!("Found OpenVINO via pkg-config"),
		Err(e) => panic!("Unable to locate OpenVINO library: {}", e)
	};
}