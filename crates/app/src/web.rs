//! A `WKWebView` from the system WebKit, laid over GPUI's view the way the
//! terminals' Ghostty views are (decisions 0008, 0025). The editor shows
//! HTML files in one.

use std::path::Path;

use gpui_kit::{Bounds, Pixels, Window};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration};

/// Whether the editor previews `path` as a web page.
pub fn is_html(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

/// One web view, hidden until shown. Dropping it removes it from the window.
pub struct WebView {
    view: Retained<WKWebView>,
    /// GPUI's view, which the web view is a subview of.
    parent: Retained<NSView>,
}

impl WebView {
    /// A hidden web view over `window`; `None` where there is no AppKit
    /// view (tests).
    pub fn new(window: &Window) -> Option<Self> {
        let parent = crate::overlay::gpui_view(window)?;
        let mtm = MainThreadMarker::new()?;
        // SAFETY: main thread; a fresh configuration and a zero frame are
        // valid arguments.
        let view = unsafe {
            let config = WKWebViewConfiguration::new(mtm);
            let view = WKWebView::initWithFrame_configuration(mtm.alloc(), NSRect::ZERO, &config);
            // Right-click → Inspect Element, for debugging pages.
            view.setInspectable(true);
            view
        };
        view.setHidden(true);
        parent.addSubview(&view);
        Some(Self { view, parent })
    }

    /// Shows the file at `path`, letting it read the files beside it (its
    /// stylesheets, scripts and images).
    pub fn load_file(&self, path: &Path) {
        let dir = path.parent().unwrap_or(path);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let access =
            NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&dir.to_string_lossy()), true);
        // SAFETY: both are file URLs; main thread.
        unsafe { self.view.loadFileURL_allowingReadAccessToURL(&url, &access) };
    }

    /// Shows `html` with no base URL: relative links don't resolve. For files
    /// on another machine.
    pub fn load_html(&self, html: &str) {
        // SAFETY: main thread.
        unsafe { self.view.loadHTMLString_baseURL(&NSString::from_str(html), None) };
    }

    pub fn reload(&self) {
        // SAFETY: main thread.
        unsafe { self.view.reload() };
    }

    /// Places the view at `bounds`, in GPUI's window coordinates.
    pub fn set_frame(&self, bounds: Bounds<Pixels>) {
        // AppKit's y axis points up, from the bottom of the parent.
        let parent_height = self.parent.bounds().size.height;
        let (x, y) = (f64::from(bounds.origin.x), f64::from(bounds.origin.y));
        let (width, height) = (
            f64::from(bounds.size.width),
            f64::from(bounds.size.height),
        );
        let frame = NSRect::new(
            NSPoint::new(x, parent_height - y - height),
            NSSize::new(width, height),
        );
        if self.view.frame() != frame {
            self.view.setFrame(frame);
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if self.view.isHidden() == visible {
            self.view.setHidden(!visible);
        }
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        self.view.removeFromSuperview();
    }
}
