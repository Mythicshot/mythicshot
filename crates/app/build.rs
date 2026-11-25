//! On Windows, embed the app icon and version details in mythicshot.exe, so
//! Explorer, the taskbar and the file's Properties show them.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/mythicshot.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/mythicshot.ico")
            .set("ProductName", "Mythicshot")
            .set("CompanyName", "Mythicshot, LLC")
            .set("FileDescription", "Mythicshot - screenshots and GIF recordings")
            .set("LegalCopyright", "Copyright (c) 2026 Mythicshot, LLC. Licensed under the GNU AGPL 3.0 or later.");
        if let Err(e) = res.compile() {
            // A missing Windows SDK should not stop a build; the exe just has no icon.
            println!("cargo:warning=could not embed the icon and version details: {e}");
        }
    }
}
