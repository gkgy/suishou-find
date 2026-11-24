//! Resolve installed macOS fonts through CoreText; never bundle Apple's fonts.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::sync::{Arc, OnceLock};

struct SystemFont {
    bytes: memmap2::Mmap,
    index: u32,
    variable: bool,
    name: String,
}
static WESTERN: OnceLock<Option<memmap2::Mmap>> = OnceLock::new();
static CHINESE: OnceLock<Option<SystemFont>> = OnceLock::new();

#[cfg(target_os = "macos")]
fn font_path(name: &str) -> Option<std::path::PathBuf> {
    use std::ffi::{CString, c_void};
    type Ref = *const c_void;
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontCreateWithName(name: Ref, size: f64, matrix: Ref) -> Ref;
        fn CTFontCopyAttribute(font: Ref, attribute: Ref) -> Ref;
        static kCTFontURLAttribute: Ref;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: Ref,
            text: *const std::ffi::c_char,
            encoding: u32,
        ) -> Ref;
        fn CFURLGetFileSystemRepresentation(
            url: Ref,
            resolve: u8,
            buffer: *mut u8,
            length: isize,
        ) -> u8;
        fn CFRelease(value: Ref);
    }
    let name = CString::new(name).ok()?;
    unsafe {
        let string = CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), 0x08000100);
        if string.is_null() {
            return None;
        }
        let font = CTFontCreateWithName(string, 16.0, std::ptr::null());
        CFRelease(string);
        if font.is_null() {
            return None;
        }
        let url = CTFontCopyAttribute(font, kCTFontURLAttribute);
        CFRelease(font);
        if url.is_null() {
            return None;
        }
        let mut buffer = [0u8; 4096];
        let success =
            CFURLGetFileSystemRepresentation(url, 1, buffer.as_mut_ptr(), buffer.len() as isize);
        CFRelease(url);
        if success == 0 {
            return None;
        }
        let length = buffer.iter().position(|byte| *byte == 0)?;
        Some(std::path::PathBuf::from(
            std::str::from_utf8(&buffer[..length]).ok()?,
        ))
    }
}
#[cfg(not(target_os = "macos"))]
fn font_path(_: &str) -> Option<std::path::PathBuf> {
    None
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn table(bytes: &[u8], face: usize, tag: &[u8; 4]) -> Option<usize> {
    let count = u16_at(bytes, face.checked_add(4)?)? as usize;
    for n in 0..count {
        let record = face.checked_add(12)?.checked_add(n.checked_mul(16)?)?;
        if bytes.get(record..record.checked_add(4)?)? == tag {
            return Some(u32_at(bytes, record.checked_add(8)?)? as usize);
        }
    }
    None
}
fn named_face(bytes: &[u8], wanted: &str) -> Option<(u32, bool)> {
    let collection = bytes.get(..4)? == b"ttcf";
    let count = if collection { u32_at(bytes, 8)? } else { 1 };
    if count > 256 {
        return None;
    }
    for index in 0..count {
        let face = if collection {
            u32_at(bytes, 12 + index as usize * 4)? as usize
        } else {
            0
        };
        let Some(names) = table(bytes, face, b"name") else {
            continue;
        };
        let records = u16_at(bytes, names.checked_add(2)?)? as usize;
        let strings = names.checked_add(u16_at(bytes, names.checked_add(4)?)? as usize)?;
        for n in 0..records {
            let record = names.checked_add(6)?.checked_add(n.checked_mul(12)?)?;
            if u16_at(bytes, record.checked_add(6)?)? != 6 {
                continue;
            }
            let platform = u16_at(bytes, record)?;
            let length = u16_at(bytes, record.checked_add(8)?)? as usize;
            let start = strings.checked_add(u16_at(bytes, record.checked_add(10)?)? as usize)?;
            let value = bytes.get(start..start.checked_add(length)?)?;
            let name = if platform == 0 || platform == 3 {
                let utf16: Vec<u16> = value
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect();
                String::from_utf16(&utf16).ok()
            } else {
                std::str::from_utf8(value).ok().map(str::to_owned)
            };
            if name.as_deref() == Some(wanted) {
                return Some((index, table(bytes, face, b"fvar").is_some()));
            }
        }
    }
    None
}
fn map_system_font(path: std::path::PathBuf) -> Option<memmap2::Mmap> {
    let file = std::fs::File::open(path).ok()?;
    // CoreText selects system-managed font files. The mapping keeps the
    // opened version alive if an OS update replaces the file.
    unsafe { memmap2::Mmap::map(&file).ok() }
}
fn load_chinese() -> Option<SystemFont> {
    // The Regular instance can resolve to the variable Medium face on newer macOS.
    let path = font_path("PingFangSC-Regular")?;
    let bytes = map_system_font(path)?;
    for name in [
        "PingFangSC-Regular",
        "PingFangSC-Medium",
        ".PingFangUITextSC-Default",
    ] {
        if let Some((index, variable)) = named_face(&bytes, name) {
            return Some(SystemFont {
                bytes,
                index,
                variable,
                name: name.to_owned(),
            });
        }
    }
    None
}
pub fn install(context: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let chinese = CHINESE.get_or_init(load_chinese);
    if let Some(chinese) = chinese {
        let mut data = FontData::from_static(&chinese.bytes);
        data.index = chinese.index;
        if chinese.variable {
            data.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", 400.0)]);
        }
        fonts
            .font_data
            .insert("macos-chinese".to_owned(), Arc::new(data));
        eprintln!(
            "Chinese font: {} face={} weight=400",
            chinese.name, chinese.index
        );
    } else {
        // Older systems retain the correctly selected Simplified Chinese face.
        for path in [
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/STHeiti Medium.ttc",
        ] {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let Some((index, _)) = named_face(&bytes, "STHeitiSC-Light")
                .or_else(|| named_face(&bytes, "STHeitiSC-Medium"))
            else {
                continue;
            };
            let mut data = FontData::from_owned(bytes);
            data.index = index;
            fonts
                .font_data
                .insert("macos-chinese".to_owned(), Arc::new(data));
            break;
        }
    }
    if let Some(bytes) =
        WESTERN.get_or_init(|| font_path(".AppleSystemUIFont").and_then(map_system_font))
    {
        fonts.font_data.insert(
            "macos-system".to_owned(),
            Arc::new(FontData::from_static(bytes)),
        );
    }
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        let names = fonts.families.entry(family.clone()).or_default();
        if fonts.font_data.contains_key("macos-chinese") {
            names.insert(0, "macos-chinese".to_owned());
        }
        if family == FontFamily::Proportional && fonts.font_data.contains_key("macos-system") {
            names.insert(0, "macos-system".to_owned());
        }
    }
    // Share the installed bytes across weights; no duplicated font collections.
    for (family, suffix, weight, postscript) in [
        ("InterfaceMedium", "medium", 500.0, "PingFangSC-Medium"),
        (
            "InterfaceSemibold",
            "semibold",
            600.0,
            "PingFangSC-Semibold",
        ),
    ] {
        let mut names = Vec::new();
        if let Some(system) = fonts.font_data.get("macos-system") {
            let mut data = system.as_ref().clone();
            data.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", weight)]);
            let name = format!("macos-system-{suffix}");
            fonts.font_data.insert(name.clone(), Arc::new(data));
            names.push(name);
        }
        if let Some(chinese) = chinese {
            let face = if chinese.variable {
                Some((chinese.index, true))
            } else {
                named_face(&chinese.bytes, postscript)
            };
            if let Some((index, variable)) = face {
                let mut data = FontData::from_static(&chinese.bytes);
                data.index = index;
                if variable {
                    data.tweak.coords =
                        egui::epaint::text::VariationCoords::new([(b"wght", weight)]);
                }
                let name = format!("macos-chinese-{suffix}");
                fonts.font_data.insert(name.clone(), Arc::new(data));
                names.push(name);
            }
        }
        names.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
        fonts
            .families
            .insert(FontFamily::Name(family.into()), names);
    }
    context.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_fonts_are_rejected() {
        for bytes in [
            vec![],
            b"ttcf".to_vec(),
            vec![0; 64],
            b"ttcf\0\0\0\0\xff\xff\xff\xff".to_vec(),
        ] {
            assert!(named_face(&bytes, "PingFangSC-Regular").is_none());
        }
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn installed_chinese_face_is_selected_by_name() {
        let font = load_chinese().expect("installed macOS PingFang font");
        assert!(font.name.contains("SC"));
        assert_eq!(
            named_face(&font.bytes, &font.name),
            Some((font.index, font.variable))
        );
    }
}

#[cfg(test)]
mod rendering_tests {
    use eframe::egui::FontFamily;
    #[test]
    #[cfg(target_os = "macos")]
    fn chinese_glyphs_have_visible_meshes() {
        let context = eframe::egui::Context::default();
        super::install(&context);
        context.begin_pass(Default::default());
        {
            for family in [
                FontFamily::Proportional,
                FontFamily::Name("InterfaceMedium".into()),
                FontFamily::Name("InterfaceSemibold".into()),
            ] {
                for size in [13.0, 15.0, 22.0] {
                    let galley = context.fonts_mut(|fonts| {
                        fonts.layout_no_wrap(
                            "随手找索引我的资料快捷键设置图片修改日期".to_owned(),
                            eframe::egui::FontId::new(size, family.clone()),
                            eframe::egui::Color32::BLACK,
                        )
                    });
                    assert!(
                        galley
                            .rows
                            .iter()
                            .all(|row| row.visuals.mesh.vertices.len() >= 4 * 20),
                        "Chinese characters must produce actual visible glyphs, not just advances"
                    );
                }
            }
        }
        let mut output = context.end_pass();
        output.textures_delta.clear();
    }
}
