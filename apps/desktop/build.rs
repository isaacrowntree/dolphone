fn main() {
    // Fluent reads as native on the GNOME and KDE desktops a Flipper OS desktop
    // profile is likely to be, and follows the system's light or dark setting.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("compile ui/app.slint");
}
