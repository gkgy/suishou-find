//! macOS 26 PingFang uses Apple's hvgl outlines, which skrifa cannot read.
//! Ask CoreText for those outlines, then use the normal epaint atlas/cache.
#[allow(unsafe_code)]
mod native {
use std::{cell::RefCell, collections::HashMap, ffi::c_void};
use vello_cpu::kurbo;
type Ref = *const c_void;
#[repr(C)]
struct Point { x: f64, y: f64 }
#[repr(C)]
struct Element { kind: i32, points: *const Point }
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontCreateWithName(name: Ref, size: f64, matrix: Ref) -> Ref;
    fn CTFontCreatePathForGlyph(font: Ref, glyph: u16, matrix: Ref) -> Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(allocator: Ref, text: *const std::ffi::c_char, encoding: u32) -> Ref;
    fn CFRelease(value: Ref);
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPathApply(path: Ref, info: *mut c_void, callback: unsafe extern "C" fn(*mut c_void, *const Element));
}
struct Font(Ref);
impl Drop for Font { fn drop(&mut self) { unsafe { CFRelease(self.0); } } }
thread_local! { static FONTS: RefCell<HashMap<(u32, u16), Font>> = RefCell::new(HashMap::new()); }
struct Pen<'a> { path: &'a mut kurbo::BezPath, offset: f64 }
unsafe extern "C" fn apply(info: *mut c_void, element: *const Element) {
    // CoreGraphics supplies the documented 1/1/2/3/0 points for these elements.
    unsafe {
        let pen = &mut *info.cast::<Pen<'_>>();
        let element = &*element;
        let point = |n| { let p = &*element.points.add(n); (p.x + pen.offset, -p.y) };
        match element.kind {
            0 => pen.path.move_to(point(0)),
            1 => pen.path.line_to(point(0)),
            2 => pen.path.quad_to(point(0), point(1)),
            3 => pen.path.curve_to(point(0), point(1), point(2)),
            4 => pen.path.close_path(),
            _ => {},
        }
    }
}
pub(crate) fn outline(glyph: u32, scale: f32, offset: f64, weight: u16, path: &mut kurbo::BezPath) -> Option<()> {
    let glyph = u16::try_from(glyph).ok()?;
    FONTS.with(|fonts| {
        let mut fonts = fonts.borrow_mut();
        let key = (scale.to_bits(), weight);
        if !fonts.contains_key(&key) {
            let font = unsafe {
                let postscript = match weight { 500 => c"PingFangSC-Medium", 600 => c"PingFangSC-Semibold", _ => c"PingFangSC-Regular" };
                let name = CFStringCreateWithCString(std::ptr::null(), postscript.as_ptr(), 0x08000100);
                if name.is_null() { return None; }
                let font = CTFontCreateWithName(name, f64::from(scale), std::ptr::null());
                CFRelease(name);
                font
            };
            if font.is_null() { return None; }
            fonts.insert(key, Font(font));
        }
        unsafe {
            let cg_path = CTFontCreatePathForGlyph(fonts.get(&key)?.0, glyph, std::ptr::null());
            if cg_path.is_null() { return None; }
            let mut pen = Pen { path, offset };
            CGPathApply(cg_path, (&mut pen as *mut Pen<'_>).cast(), apply);
            CFRelease(cg_path);
        }
        Some(())
    })
}

}
pub(super) use native::outline;
