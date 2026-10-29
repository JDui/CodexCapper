//! DirectWrite text and antialiased, per-pixel-alpha taskbar rendering.
//!
//! The bitmap remains premultiplied BGRA from WIC through UpdateLayeredWindow.
//! In particular there is no colour key or ClearType RGB fringe at transparent
//! edges, and no second text pass cutting a glyph at the progress boundary.

use std::cell::RefCell;
use std::sync::Arc;

use tauri::{AppHandle, Manager};
use windows::core::{w, Error, Result, HRESULT, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, UpdateLayeredWindow, ULW_ALPHA};

use crate::model::{Source, WindowSnapshot};
use crate::state::AppState;

const CAPSULE_PADDING: f32 = 7.0;
const CAPSULE_TEXT_GAP: f32 = 2.0;
const CAPSULE_INK: u32 = 0x111827;

thread_local! {
    static FACTORIES: RefCell<Option<Initialized>> = const { RefCell::new(None) };
}

struct Apartment(bool);

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

#[derive(Clone)]
struct Factories {
    d2d: ID2D1Factory,
    text: IDWriteFactory,
    wic: IWICImagingFactory,
}

struct Initialized {
    factories: Factories,
    // COM objects above are released before this thread's initialization.
    _apartment: Apartment,
}

fn factories() -> Result<Factories> {
    FACTORIES.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            unsafe {
                let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                // A host already using MTA is initialized too; do not balance
                // its initialization with our own CoUninitialize.
                if hr.is_err() && hr != HRESULT(0x80010106u32 as i32) {
                    return Err(Error::from_hresult(hr));
                }
                let apartment = Apartment(hr.is_ok());
                let factories = Factories {
                    d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                    text: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
                    wic: CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?,
                };
                *slot = Some(Initialized {
                    factories,
                    _apartment: apartment,
                });
            }
        }
        Ok(slot.as_ref().unwrap().factories.clone())
    })
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut core::ffi::c_void,
}

impl Surface {
    unsafe fn new(width: i32, height: i32) -> Result<Self> {
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return Err(Error::from_thread());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = core::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                let _ = DeleteDC(dc);
                return Err(error);
            }
        };
        let previous = SelectObject(dc, bitmap.into());
        Ok(Self {
            dc,
            bitmap,
            previous,
            bits,
        })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn color(rgb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 255) as f32 / 255.0,
        g: ((rgb >> 8) & 255) as f32 / 255.0,
        b: (rgb & 255) as f32 / 255.0,
        a: 1.0,
    }
}

struct Canvas<'a> {
    factory: &'a Factories,
    target: ID2D1RenderTarget,
    family: Vec<u16>,
    caption_weight: DWRITE_FONT_WEIGHT,
}

struct CaptionFit {
    size: f32,
    label_width: f32,
    percentage_width: f32,
    reset_width: f32,
}

impl Canvas<'_> {
    unsafe fn fill(&self, area: D2D_RECT_F, value: u32, radius: f32) -> Result<()> {
        let brush = self.target.CreateSolidColorBrush(&color(value), None)?;
        self.target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: area,
                radiusX: radius,
                radiusY: radius,
            },
            &brush,
        );
        Ok(())
    }

    unsafe fn progress(
        &self,
        area: D2D_RECT_F,
        remaining: f32,
        label: &str,
        stale: bool,
        dark: bool,
        compact: bool,
    ) -> Result<()> {
        let area = if compact {
            // The outside silhouette is painted exactly once, independently
            // of the quota. Every coloured segment stays inside this frame.
            self.fill(
                area,
                if dark { 0x68778d } else { 0x8791a2 },
                (area.bottom - area.top) / 2.0,
            )?;
            rect(
                area.left + 1.0,
                area.top + 1.0,
                area.right - 1.0,
                area.bottom - 1.0,
            )
        } else {
            area
        };
        let radius = (area.bottom - area.top) / 2.0;
        let edge = area.left + (area.right - area.left) * remaining / 100.0;
        // Draw each segment only once. This preserves the same edge alpha on
        // the filled and empty halves instead of accumulating two AA masks.
        self.target.PushAxisAlignedClip(
            &rect(edge, area.top, area.right, area.bottom),
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        let empty = if compact {
            if dark {
                0xc7cbd4
            } else {
                0xeceef2
            }
        } else if dark {
            0x383b42
        } else {
            0xe6e7eb
        };
        let result = self.fill(area, empty, radius);
        self.target.PopAxisAlignedClip();
        result?;

        let (start, end) = if stale {
            (0xb5bac5, 0xb5bac5)
        } else if compact && label == "7d" {
            if dark {
                (0xaaa4cc, 0xcec1e0)
            } else {
                (0xada6cd, 0xd2c6e6)
            }
        } else if compact {
            if dark {
                (0x7caeb4, 0xa5cbc0)
            } else {
                (0x83aeb2, 0xabd0c4)
            }
        } else if label == "7d" {
            if dark {
                (0x9691bd, 0xc5b5dc)
            } else {
                (0x77729c, 0xb3a5c9)
            }
        } else if dark {
            (0x6da5af, 0xa6c9bd)
        } else {
            (0x467f88, 0x8bb6ad)
        };
        let stops = self.target.CreateGradientStopCollection(
            &[
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: color(start),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: color(end),
                },
            ],
            D2D1_GAMMA_2_2,
            D2D1_EXTEND_MODE_CLAMP,
        )?;
        let mut properties = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES::default();
        properties.startPoint.X = area.left;
        properties.startPoint.Y = area.top;
        properties.endPoint.X = area.right;
        properties.endPoint.Y = area.top;
        let brush = self
            .target
            .CreateLinearGradientBrush(&properties, None, &stops)?;
        self.target.PushAxisAlignedClip(
            &rect(area.left, area.top, edge, area.bottom),
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        self.target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: area,
                radiusX: radius,
                radiusY: radius,
            },
            &brush,
        );
        self.target.PopAxisAlignedClip();
        Ok(())
    }

    unsafe fn format(&self, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
        let format = self.factory.text.CreateTextFormat(
            PCWSTR(self.family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("zh-CN"),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(format)
    }

    unsafe fn measure(&self, value: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<f32> {
        let format = self.format(size, weight)?;
        let wide: Vec<u16> = value.encode_utf16().collect();
        let layout = self
            .factory
            .text
            .CreateTextLayout(&wide, &format, 2048.0, 32.0)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        Ok(metrics.widthIncludingTrailingWhitespace)
    }

    unsafe fn text(
        &self,
        value: &str,
        area: D2D_RECT_F,
        size: f32,
        weight: DWRITE_FONT_WEIGHT,
        value_color: u32,
        right: bool,
    ) -> Result<()> {
        let format = self.format(size, weight)?;
        if right {
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        }
        let brush = self
            .target
            .CreateSolidColorBrush(&color(value_color), None)?;
        self.target.DrawText(
            &value.encode_utf16().collect::<Vec<_>>(),
            &format,
            &area,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        Ok(())
    }

    unsafe fn caption_fit(
        &self,
        label: &str,
        percentage: &str,
        reset: &str,
        available: f32,
        labeled: bool,
    ) -> Result<CaptionFit> {
        let mut size = if labeled { 11.0f32 } else { 12.0f32 };
        loop {
            let label_width = if labeled {
                self.measure(label, size, self.caption_weight)? + CAPSULE_TEXT_GAP
            } else {
                0.0
            };
            let percentage_width = self.measure(percentage, size, self.caption_weight)?;
            let reset_width = self.measure(reset, size, self.caption_weight)?;
            if label_width + percentage_width + CAPSULE_TEXT_GAP + reset_width <= available
                || size <= 10.0
            {
                return Ok(CaptionFit {
                    size,
                    label_width,
                    percentage_width,
                    reset_width,
                });
            }
            size -= 0.25;
        }
    }

    unsafe fn capsule(
        &self,
        area: D2D_RECT_F,
        label: &str,
        window: &WindowSnapshot,
        style: &str,
        now: chrono::DateTime<chrono::Utc>,
        dark: bool,
        labeled: bool,
    ) -> Result<()> {
        let remaining = (100.0 - window.utilization).clamp(0.0, 100.0);
        self.progress(area, remaining, label, window.stale, dark, true)?;
        let estimate = match window.source {
            Source::Official => "",
            Source::Estimate => "估",
            Source::EstimateLocalOnly => "估*",
        };
        let percentage = format!("{remaining:.0}%{estimate}");
        let reset = crate::reset_time::format(window.resets_at, label, style, now, true);
        let reset = if reset.is_empty() { "--" } else { &reset };
        let available = area.right - area.left - 2.0 * CAPSULE_PADDING;
        let CaptionFit {
            size,
            label_width,
            percentage_width,
            reset_width,
        } = self.caption_fit(label, &percentage, reset, available, labeled)?;
        let left = area.left + CAPSULE_PADDING;
        if labeled {
            self.text(
                label,
                rect(left, area.top, left + label_width, area.bottom),
                size,
                self.caption_weight,
                CAPSULE_INK,
                false,
            )?;
        }
        let pleft = left + label_width;
        self.text(
            &percentage,
            rect(pleft, area.top, pleft + percentage_width + 0.5, area.bottom),
            size,
            self.caption_weight,
            CAPSULE_INK,
            false,
        )?;
        let rright = area.right - CAPSULE_PADDING;
        self.text(
            reset,
            rect(
                (rright - reset_width - 0.5).max(pleft + percentage_width + CAPSULE_TEXT_GAP),
                area.top,
                rright,
                area.bottom,
            ),
            size,
            self.caption_weight,
            CAPSULE_INK,
            true,
        )?;
        Ok(())
    }
}

unsafe fn create_canvas<'a>(
    factory: &'a Factories,
    family: &str,
    width: i32,
    height: i32,
    dpi: f32,
) -> Result<(Canvas<'a>, IWICBitmap)> {
    let bitmap = factory.wic.CreateBitmap(
        width as u32,
        height as u32,
        &GUID_WICPixelFormat32bppPBGRA,
        WICBitmapCacheOnLoad,
    )?;
    let target = factory.d2d.CreateWicBitmapRenderTarget(
        &bitmap,
        &D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        },
    )?;
    target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    let system_params = factory.text.CreateRenderingParams()?;
    let params = factory.text.CreateCustomRenderingParams(
        system_params.GetGamma(),
        0.0,
        0.0,
        DWRITE_PIXEL_GEOMETRY_FLAT,
        DWRITE_RENDERING_MODE_NATURAL,
    )?;
    target.SetTextRenderingParams(&params);
    Ok((
        Canvas {
            factory,
            target,
            family: family.encode_utf16().chain(Some(0)).collect(),
            caption_weight: if family.eq_ignore_ascii_case("Georgia") {
                DWRITE_FONT_WEIGHT_NORMAL
            } else {
                DWRITE_FONT_WEIGHT_MEDIUM
            },
        },
        bitmap,
    ))
}

/// Paints a complete transparent surface and atomically presents it to the
/// existing WS_CHILD. It does not resize, reparent or activate that window.
pub fn render(hwnd: HWND, app: &AppHandle) -> Result<()> {
    unsafe {
        let mut bounds = RECT::default();
        GetClientRect(hwnd, &mut bounds)?;
        let (width, height) = (bounds.right, bounds.bottom);
        if width <= 0 || height <= 0 {
            return Ok(());
        }
        if width > 4096 || height > 4096 {
            return Err(Error::from_hresult(HRESULT(0x80070057u32 as i32)));
        }
        let factory = factories()?;
        let dpi = GetDpiForWindow(hwnd).max(96) as f32;
        let scale = dpi / 96.0;
        let state = app.state::<Arc<AppState>>();
        let settings = state.settings.get();
        let payload = state.payload.read().clone();
        let dark = crate::theme::resolve(&settings.appearance.theme) == "dark";
        let family = crate::fonts::native_family(&settings.appearance.font_family);
        let (canvas, bitmap) = create_canvas(&factory, family, width, height, dpi)?;
        let w = width as f32 / scale;
        let h = height as f32 / scale;
        canvas.target.BeginDraw();
        canvas.target.Clear(None);
        let result = draw_scene(&canvas, payload.as_ref(), &settings, w, h, dark);
        let ended = canvas.target.EndDraw(None, None);
        result?;
        ended?;
        let surface = Surface::new(width, height)?;
        let bytes = core::slice::from_raw_parts_mut(
            surface.bits as *mut u8,
            width as usize * height as usize * 4,
        );
        bitmap.CopyPixels(core::ptr::null(), width as u32 * 4, bytes)?;
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&SIZE {
                cx: width,
                cy: height,
            }),
            Some(surface.dc),
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )?;
        #[cfg(debug_assertions)]
        dump_pixels(bytes, width, height, family, settings.widget.compact_mode);
        Ok(())
    }
}

unsafe fn draw_scene(
    canvas: &Canvas<'_>,
    payload: Option<&crate::model::UsagePayload>,
    settings: &crate::settings::Settings,
    width: f32,
    height: f32,
    dark: bool,
) -> Result<()> {
    let compact = settings.widget.compact_mode;
    let rows = crate::taskbar_embed::rows(payload);
    let foreground = if dark { 0xf5f5f5 } else { 0x202020 };
    let muted = if dark { 0xbababa } else { 0x666666 };
    let now = chrono::Utc::now();
    if !compact {
        canvas.fill(
            rect(0.0, 0.0, width, height),
            if dark { 0x232323 } else { 0xfafafa },
            8.0,
        )?;
    }
    if rows.is_empty() {
        if compact {
            canvas.progress(rect(0.0, 0.0, width, height), 0.0, "5h", false, dark, true)?;
        }
        let status = payload
            .and_then(|p| p.providers.iter().find(|p| p.enabled))
            .and_then(|p| p.detail.as_deref())
            .unwrap_or(if payload.is_none() {
                "加载额度…"
            } else {
                "暂无额度数据"
            });
        canvas.text(
            status,
            rect(
                if compact { CAPSULE_PADDING } else { 5.0 },
                0.0,
                width - if compact { CAPSULE_PADDING } else { 5.0 },
                height,
            ),
            if compact { 12.0 } else { 11.0 },
            if compact {
                canvas.caption_weight
            } else {
                DWRITE_FONT_WEIGHT_MEDIUM
            },
            if compact { CAPSULE_INK } else { foreground },
            false,
        )?;
        return Ok(());
    }
    if compact {
        let top = (height - crate::taskbar_embed::compact_height(rows.len()) as f32) / 2.0;
        let capsule_height = if rows.len() == 1 {
            crate::monitor::COMPACT_SINGLE_LOGICAL_H as f32
        } else {
            18.0
        };
        for (i, (label, window)) in rows.iter().enumerate() {
            let y = top + i as f32 * 20.0;
            canvas.capsule(
                rect(0.0, y, width, y + capsule_height),
                label,
                window,
                &settings.widget.reset_time_style,
                now,
                dark,
                rows.len() > 1,
            )?;
        }
    } else {
        let header = 13.0;
        let row_height = 16.0f32.min((height - header - 2.0) / rows.len() as f32);
        let top = (height - header - row_height * rows.len() as f32) / 2.0;
        let provider = payload.and_then(|p| p.providers.iter().find(|p| p.enabled));
        let plan = crate::taskbar_embed::plan_name(provider.and_then(|p| p.plan_type.as_deref()));
        canvas.text(
            &plan,
            rect(10.0, top, 130.0, top + header),
            11.0,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            foreground,
            false,
        )?;
        canvas.text(
            "剩余额度",
            rect(width - 64.0, top, width - 8.0, top + header),
            10.5,
            DWRITE_FONT_WEIGHT_NORMAL,
            muted,
            true,
        )?;
        for (i, (label, window)) in rows.iter().enumerate() {
            let y = top + header + i as f32 * row_height;
            let remaining = (100.0 - window.utilization).clamp(0.0, 100.0);
            canvas.text(
                label,
                rect(10.0, y, 32.0, y + row_height),
                11.0,
                DWRITE_FONT_WEIGHT_MEDIUM,
                muted,
                false,
            )?;
            let bar_y = y + row_height / 2.0;
            canvas.progress(
                rect(36.0, bar_y - 3.0, 102.0, bar_y + 3.0),
                remaining,
                label,
                window.stale,
                dark,
                false,
            )?;
            let estimate = if window.source == Source::Official {
                ""
            } else {
                "估"
            };
            let percentage = format!("{remaining:.0}%{estimate}");
            let percent_color = if remaining <= 10.0 {
                if dark {
                    0xef7373
                } else {
                    0xc62828
                }
            } else if remaining <= 30.0 {
                if dark {
                    0xf5b84e
                } else {
                    0xa17020
                }
            } else {
                foreground
            };
            let mut percent_size = 11.0;
            while percent_size > 10.0
                && canvas.measure(&percentage, percent_size, DWRITE_FONT_WEIGHT_SEMI_BOLD)? > 38.0
            {
                percent_size -= 0.25;
            }
            canvas.text(
                &percentage,
                rect(110.0, y, 148.0, y + row_height),
                percent_size,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                percent_color,
                false,
            )?;
            let reset = crate::reset_time::format(
                window.resets_at,
                label,
                &settings.widget.reset_time_style,
                now,
                false,
            );
            let mut reset_size = 11.0;
            while reset_size > 10.0
                && canvas.measure(&reset, reset_size, DWRITE_FONT_WEIGHT_MEDIUM)? > width - 158.0
            {
                reset_size -= 0.25;
            }
            canvas.text(
                &reset,
                rect(150.0, y, width - 8.0, y + row_height),
                reset_size,
                DWRITE_FONT_WEIGHT_MEDIUM,
                muted,
                true,
            )?;
        }
    }
    Ok(())
}

/// Opt-in debug evidence: raw premultiplied pixels plus dimensions. This path
/// is never active in release builds and does not affect the product UI.
#[cfg(debug_assertions)]
fn dump_pixels(bytes: &[u8], width: i32, height: i32, family: &str, compact: bool) {
    let Some(directory) = std::env::var_os("CODEX_CAPPER_RENDER_DUMP") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let stem = format!(
        "{}-{}x{}-{}",
        family.replace(' ', "-"),
        width,
        height,
        if compact { "compact" } else { "normal" }
    );
    let _ = std::fs::write(directory.join(format!("{stem}.bgra")), bytes);
    let partial = bytes
        .chunks_exact(4)
        .filter(|p| p[3] > 0 && p[3] < 255)
        .count();
    let transparent = bytes.chunks_exact(4).filter(|p| p[3] == 0).count();
    let _ = std::fs::write(directory.join(format!("{stem}.json")), format!("{{\"width\":{width},\"height\":{height},\"premultiplied\":true,\"partial_alpha_pixels\":{partial},\"transparent_pixels\":{transparent}}}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderSnapshot, UsagePayload};

    // Drawing tests stalled when run across short-lived COM test threads.
    // Keep graphics on one persistent thread, like the app's actual UI thread,
    // while Rust runs unrelated tests concurrently. A mutex alone would still
    // repeatedly destroy and recreate different COM apartments.
    fn on_graphics_thread(check: fn()) {
        type Work = Box<dyn FnOnce() + Send>;
        static GRAPHICS: std::sync::OnceLock<std::sync::mpsc::Sender<Work>> =
            std::sync::OnceLock::new();
        let queue = GRAPHICS.get_or_init(|| {
            let (send, receive) = std::sync::mpsc::channel::<Work>();
            std::thread::Builder::new()
                .name("taskbar-render-tests".into())
                .spawn(move || {
                    for work in receive {
                        work();
                    }
                })
                .expect("graphics test thread");
            send
        });
        let (completed, result) = std::sync::mpsc::channel();
        assert!(
            queue
                .send(Box::new(move || {
                    let checked = std::panic::catch_unwind(check);
                    let _ = completed.send(checked);
                }))
                .is_ok(),
            "graphics test queue"
        );
        if let Err(panic) = result
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("graphics test result within 30 seconds")
        {
            std::panic::resume_unwind(panic);
        }
    }

    fn payload(pair: bool) -> UsagePayload {
        let mut provider = ProviderSnapshot::empty("codex", "Codex");
        let mut window =
            WindowSnapshot::official(0.0, Some(chrono::Utc::now() + chrono::Duration::days(3)));
        window.source = Source::EstimateLocalOnly;
        if pair {
            provider.five_hour = Some(window.clone());
        }
        window.utilization = 93.0;
        provider.weekly = Some(window);
        UsagePayload {
            providers: vec![provider],
            generated_at: chrono::Utc::now(),
            onboarding: false,
        }
    }

    #[test]
    fn transparent_edges_use_fractional_premultiplied_alpha_at_all_supported_scales() {
        on_graphics_thread(check_transparent_edges);
    }

    fn check_transparent_edges() {
        let factory = factories().expect("COM and drawing factories");
        let mut count = 0;
        for font in ["system", "serif", "mono"] {
            for dpi in [96.0f32, 120.0, 144.0, 192.0] {
                for pair in [false, true] {
                    for theme in ["light", "dark"] {
                        let mut settings = crate::settings::Settings::default();
                        settings.appearance.font_family = font.into();
                        settings.appearance.theme = theme.into();
                        settings.widget.compact_mode = true;
                        let scale = dpi / 96.0;
                        let width =
                            (crate::monitor::COMPACT_LOGICAL_W as f32 * scale).round() as i32;
                        let height = (crate::taskbar_embed::compact_height(if pair { 2 } else { 1 })
                            as f32
                            * scale)
                            .round() as i32;
                        unsafe {
                            let (canvas, bitmap) = create_canvas(
                                &factory,
                                crate::fonts::native_family(font),
                                width,
                                height,
                                dpi,
                            )
                            .expect("WIC render target");
                            canvas.target.BeginDraw();
                            canvas.target.Clear(None);
                            draw_scene(
                                &canvas,
                                Some(&payload(pair)),
                                &settings,
                                width as f32 / scale,
                                height as f32 / scale,
                                theme == "dark",
                            )
                            .expect("scene render");
                            canvas.target.EndDraw(None, None).expect("finish scene");
                            let mut bytes = vec![0u8; width as usize * height as usize * 4];
                            bitmap
                                .CopyPixels(core::ptr::null(), width as u32 * 4, &mut bytes)
                                .expect("read WIC pixels");
                            let pixels: Vec<_> = bytes.chunks_exact(4).collect();
                            assert!(
                                pixels.iter().any(|p| p[3] == 0),
                                "transparent corners: {font}/{dpi}/{pair}/{theme}"
                            );
                            assert!(
                                pixels.iter().any(|p| p[3] == 255),
                                "opaque interior: {font}/{dpi}/{pair}/{theme}"
                            );
                            let fractional =
                                pixels.iter().filter(|p| p[3] > 0 && p[3] < 255).count();
                            assert!(
                                fractional > 0,
                                "antialiased silhouette: {font}/{dpi}/{pair}/{theme}"
                            );
                            assert!(
                                pixels
                                    .iter()
                                    .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]),
                                "premultiplied channels: {font}/{dpi}/{pair}/{theme}"
                            );
                        }
                        count += 1;
                    }
                }
            }
        }
        assert_eq!(count, 48);
    }

    #[test]
    fn long_captions_fit_all_three_fonts_without_overlapping_at_each_scale() {
        on_graphics_thread(check_long_captions);
    }

    fn check_long_captions() {
        let factory = factories().expect("drawing factories");
        for font in ["system", "serif", "mono"] {
            for dpi in [96.0f32, 120.0, 144.0, 192.0] {
                let scale = dpi / 96.0;
                let width = (crate::monitor::COMPACT_LOGICAL_W as f32 * scale).round() as i32;
                unsafe {
                    let (canvas, _) = create_canvas(
                        &factory,
                        crate::fonts::native_family(font),
                        width,
                        (38.0 * scale).round() as i32,
                        dpi,
                    )
                    .expect("WIC target");
                    let available = width as f32 / scale - 2.0 * CAPSULE_PADDING;
                    for label in ["5h", "7d"] {
                        for labeled in [false, true] {
                            let fit = canvas
                                .caption_fit(label, "100%估*", "11/05 23:59", available, labeled)
                                .expect("measure actual glyph metrics");
                            let required = fit.label_width
                                + fit.percentage_width
                                + CAPSULE_TEXT_GAP
                                + fit.reset_width;
                            assert!((10.0..=if labeled { 11.0 } else { 12.0 }).contains(&fit.size));
                            assert!(required <= available + 0.01, "{font}/{dpi}/{label}/{labeled} needs {required} but has {available}");
                            let official = canvas
                                .caption_fit(label, "100%", "11/05 23:59", available, labeled)
                                .expect("official caption metrics");
                            assert_eq!(official.size, if labeled { 11.0 } else { 12.0 }, "common captions retain their intended size: {font}/{dpi}/{label}/{labeled}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn silhouette_is_identical_at_every_fill_and_endpoints_are_distinct() {
        on_graphics_thread(check_silhouette);
    }

    fn check_silhouette() {
        let factory = factories().expect("drawing factories");
        for dpi in [96.0f32, 120.0, 144.0, 192.0] {
            for logical_height in [18.0f32, crate::monitor::COMPACT_SINGLE_LOGICAL_H as f32] {
                for dark in [false, true] {
                    let scale = dpi / 96.0;
                    let width = (crate::monitor::COMPACT_LOGICAL_W as f32 * scale).round() as i32;
                    let height = (logical_height * scale).round() as i32;
                    let mut previous: Option<Vec<u8>> = None;
                    let mut silhouette: Option<Vec<u8>> = None;
                    let mut saturated = Vec::new();
                    for remaining in [0.0f32, 1.0, 50.0, 99.0, 100.0] {
                        unsafe {
                            let (canvas, bitmap) = create_canvas(
                                &factory,
                                crate::fonts::native_family("system"),
                                width,
                                height,
                                dpi,
                            )
                            .expect("WIC target");
                            canvas.target.BeginDraw();
                            canvas.target.Clear(None);
                            canvas
                                .progress(
                                    rect(0.0, 0.0, width as f32 / scale, height as f32 / scale),
                                    remaining,
                                    "5h",
                                    false,
                                    dark,
                                    true,
                                )
                                .expect("progress shape");
                            canvas.target.EndDraw(None, None).expect("finish progress");
                            let mut pixels = vec![0u8; width as usize * height as usize * 4];
                            bitmap
                                .CopyPixels(core::ptr::null(), width as u32 * 4, &mut pixels)
                                .expect("pixel read");
                            let top = &pixels[(width as usize / 2) * 4..][..4];
                            assert!(
                                top[3] > 0 && top[2] < 160,
                                "visible outline at {remaining}%/{dpi}/{logical_height}/{dark}"
                            );
                            let alpha: Vec<_> = pixels.chunks_exact(4).map(|p| p[3]).collect();
                            if let Some(base) = &silhouette {
                                assert_eq!(&alpha, base, "exact alpha silhouette at {remaining}%/{dpi}/{logical_height}/{dark}");
                            } else {
                                silhouette = Some(alpha);
                            }
                            let colored = pixels
                                .chunks_exact(4)
                                .filter(|p| p[1] as i16 - p[2] as i16 > 25)
                                .count();
                            saturated.push(colored);
                            if let Some(before) = &previous {
                                let changed = before
                                    .chunks_exact(4)
                                    .zip(pixels.chunks_exact(4))
                                    .filter(|(a, b)| a != b)
                                    .count();
                                assert!(
                                    changed > 0,
                                    "distinct {remaining}%/{dpi}/{logical_height}/{dark}"
                                );
                            }
                            previous = Some(pixels);
                        }
                    }
                    assert_eq!(saturated[0], 0, "0% has no coloured fill");
                    assert!(
                        saturated.windows(2).all(|p| p[0] <= p[1])
                            && saturated[2] > saturated[0]
                            && saturated[4] > saturated[2],
                        "increasing fill coverage at {dpi}/{logical_height}/{dark}: {saturated:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn caption_pixels_stay_inside_seven_pixel_insets_for_all_fonts_and_scales() {
        on_graphics_thread(check_caption_pixels);
    }

    fn check_caption_pixels() {
        let factory = factories().expect("drawing factories");
        for font in ["system", "serif", "mono"] {
            for dpi in [96.0f32, 120.0, 144.0, 192.0] {
                for labeled in [false, true] {
                    let scale = dpi / 96.0;
                    let width = (crate::monitor::COMPACT_LOGICAL_W as f32 * scale).round() as i32;
                    let logical_height = if labeled {
                        18.0
                    } else {
                        crate::monitor::COMPACT_SINGLE_LOGICAL_H as f32
                    };
                    let height = (logical_height * scale).round() as i32;
                    let logical_width = width as f32 / scale;
                    let area = rect(0.0, 0.0, logical_width, height as f32 / scale);
                    unsafe {
                        let (canvas, bitmap) = create_canvas(
                            &factory,
                            crate::fonts::native_family(font),
                            width,
                            height,
                            dpi,
                        )
                        .expect("WIC target");
                        let now = chrono::Utc::now();
                        let mut window =
                            WindowSnapshot::official(0.0, Some(now + chrono::Duration::days(3)));
                        window.source = Source::EstimateLocalOnly;
                        canvas.target.BeginDraw();
                        canvas.target.Clear(None);
                        canvas
                            .progress(area, 100.0, "7d", false, true, true)
                            .expect("background");
                        canvas
                            .target
                            .EndDraw(None, None)
                            .expect("finish background");
                        let mut before = vec![0u8; width as usize * height as usize * 4];
                        bitmap
                            .CopyPixels(core::ptr::null(), width as u32 * 4, &mut before)
                            .expect("background pixels");
                        canvas.target.BeginDraw();
                        canvas.target.Clear(None);
                        canvas
                            .capsule(area, "7d", &window, "auto", now, true, labeled)
                            .expect("caption");
                        canvas.target.EndDraw(None, None).expect("finish caption");
                        let mut after = vec![0u8; before.len()];
                        bitmap
                            .CopyPixels(core::ptr::null(), width as u32 * 4, &mut after)
                            .expect("caption pixels");
                        let left = (CAPSULE_PADDING * scale).floor() as usize;
                        let right = ((logical_width - CAPSULE_PADDING) * scale).ceil() as usize;
                        let changed: Vec<_> = before
                            .chunks_exact(4)
                            .zip(after.chunks_exact(4))
                            .enumerate()
                            .filter_map(|(i, (a, b))| (a != b).then_some(i % width as usize))
                            .collect();
                        assert!(
                            !changed.is_empty(),
                            "caption visible {font}/{dpi}/{labeled}"
                        );
                        assert!(changed.iter().all(|x| *x >= left && *x < right), "text ink inside 7px inset {font}/{dpi}/{labeled}: {:?}..{:?}, expected {left}..{right}", changed.iter().min(), changed.iter().max());
                    }
                }
            }
        }
    }

    #[test]
    fn compact_pastels_and_tracks_keep_accessible_text_contrast() {
        fn luminance(value: u32) -> f64 {
            let channel = |shift| {
                let v = ((value >> shift) & 255u32) as f64 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            channel(16) * 0.2126 + channel(8) * 0.7152 + channel(0) * 0.0722
        }
        let ink = luminance(CAPSULE_INK);
        for background in [
            0x7caeb4, 0xa5cbc0, 0xaaa4cc, 0xcec1e0, 0xc7cbd4, 0x83aeb2, 0xabd0c4, 0xada6cd,
            0xd2c6e6, 0xeceef2, 0xb5bac5,
        ] {
            let contrast = (luminance(background) + 0.05) / (ink + 0.05);
            assert!(contrast >= 4.5, "#{background:06x} contrast {contrast}");
        }
    }
}
