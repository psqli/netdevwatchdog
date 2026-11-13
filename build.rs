fn main() {
    #[cfg(feature = "gui")]
    slint_build::compile("ui/app.slint").expect("ui/app.slint compiles");
}
