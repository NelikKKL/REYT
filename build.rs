fn main() {
    #[cfg(windows)]
    {
        // Иконка опциональна: если assets/icon.ico отсутствует, просто пропускаем.
        if std::path::Path::new("assets/icon.ico").exists() {
            let mut res = winres::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.compile().expect("не удалось встроить иконку exe");
        }
    }
}
