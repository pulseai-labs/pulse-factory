fn main() {
    std::process::exit(pulse_factory::run(std::env::args_os().collect()));
}
