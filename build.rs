//! Встраивает иконку и версию в exe (иконка в проводнике и на панели задач).
//! Если Windows SDK с `rc.exe` недоступен — просто предупреждаем, сборка идёт.

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "Voice Inputter");
        res.set("FileDescription", "Voice Inputter — голосовой ввод");
        if let Err(e) = res.compile() {
            println!("cargo:warning=иконка не встроена в exe: {e}");
        }
    }
}
