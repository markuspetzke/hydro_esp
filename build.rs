fn main() {
    embuild::espidf::sysenv::output();
    dotenv_build::output(dotenv_build::Config::default()).unwrap();
}
