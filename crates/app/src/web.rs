//! A `WKWebView` from the system WebKit, laid over GPUI's view the way the
//! terminals' Ghostty views are (decisions 0008, 0025). The editor shows
//! HTML files in one.

use std::path::Path;

use block2::DynBlock;
use gpui_kit::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSView, NSWorkspace};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationType,
    WKWebView, WKWebViewConfiguration,
};

/// URL schemes a page may navigate to in place: its own file, and what
/// WebKit uses for loaded strings and generated content. Any other link
/// opens in the default browser.
const IN_PLACE: [&str; 4] = ["file", "about", "data", "blob"];

define_class!(
    /// The pages' navigation delegate: keeps a page on its file, and sends
    /// links to the web out to the default browser.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GrasslandsPageNavigation"]
    struct Navigation;

    unsafe impl NSObjectProtocol for Navigation {}

    unsafe impl WKNavigationDelegate for Navigation {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            web_view: &WKWebView,
            action: &WKNavigationAction,
            decision: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            decision.call((policy_for(web_view, action),));
        }
    }
);

impl Navigation {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// Allows navigation inside frames and to in-place schemes. A clicked link
/// elsewhere opens in the default browser; any other navigation away (a
/// redirect, a script) is dropped, so a page can't launch apps on its own.
/// An in-place link that asks for a new window loads in the page instead,
/// as there is no second window to put it in.
fn policy_for(web_view: &WKWebView, action: &WKNavigationAction) -> WKNavigationActionPolicy {
    // SAFETY: main thread, inside WebKit's delegate callback.
    let (request, frame, kind) = unsafe {
        (
            action.request(),
            action.targetFrame(),
            action.navigationType(),
        )
    };
    let Some(url) = request.URL() else {
        return WKNavigationActionPolicy::Allow;
    };
    let in_place = url
        .scheme()
        .is_some_and(|scheme| IN_PLACE.contains(&scheme.to_string().to_lowercase().as_str()));
    match frame {
        // SAFETY: as above.
        Some(frame) if in_place || unsafe { !frame.isMainFrame() } => {
            return WKNavigationActionPolicy::Allow;
        }
        None if in_place => {
            // SAFETY: as above; `request` is WebKit's own.
            unsafe { web_view.loadRequest(&request) };
            return WKNavigationActionPolicy::Cancel;
        }
        _ => {}
    }
    if kind == WKNavigationType::LinkActivated && !NSWorkspace::sharedWorkspace().openURL(&url) {
        tracing::warn!(
            "web: no app opened {}",
            url.absoluteString().unwrap_or_default()
        );
    }
    WKNavigationActionPolicy::Cancel
}

/// One web view, hidden until shown. Dropping it removes it from the window.
pub struct WebView {
    view: Retained<WKWebView>,
    /// GPUI's view, which the web view is a subview of.
    parent: Retained<NSView>,
    /// The web view only holds its delegate weakly.
    _navigation: Retained<Navigation>,
}

impl WebView {
    /// A hidden web view over `window`; `None` where there is no AppKit
    /// view (tests).
    pub fn new(window: &Window) -> Option<Self> {
        let parent = crate::overlay::gpui_view(window)?;
        let mtm = MainThreadMarker::new()?;
        let navigation = Navigation::new(mtm);
        // SAFETY: main thread; a fresh configuration and a zero frame are
        // valid arguments, and `navigation` outlives the view (both are ours).
        let view = unsafe {
            let config = WKWebViewConfiguration::new(mtm);
            let view = WKWebView::initWithFrame_configuration(mtm.alloc(), NSRect::ZERO, &config);
            // Right-click → Inspect Element, for debugging pages.
            view.setInspectable(cfg!(debug_assertions));
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*navigation)));
            view
        };
        view.setHidden(true);
        parent.addSubview(&view);
        Some(Self {
            view,
            parent,
            _navigation: navigation,
        })
    }

    /// Shows the file at `path`, letting it read files under `access` (its
    /// stylesheets, scripts and images): a folder, or `path` itself.
    pub fn load_file(&self, path: &Path, access: &Path) {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let access = NSURL::fileURLWithPath_isDirectory(
            &NSString::from_str(&access.to_string_lossy()),
            access != path,
        );
        // SAFETY: both are file URLs; main thread.
        unsafe { self.view.loadFileURL_allowingReadAccessToURL(&url, &access) };
    }

    /// Shows `html` with no base URL: relative links don't resolve. For files
    /// on another machine.
    pub fn load_html(&self, html: &str) {
        // SAFETY: main thread.
        unsafe {
            self.view
                .loadHTMLString_baseURL(&NSString::from_str(html), None)
        };
    }

    /// Places the view at `bounds`, in GPUI's window coordinates.
    pub fn set_frame(&self, bounds: Bounds<Pixels>) {
        // AppKit's y axis points up, from the bottom of the parent.
        let parent_height = self.parent.bounds().size.height;
        let (x, y) = (f64::from(bounds.origin.x), f64::from(bounds.origin.y));
        let (width, height) = (f64::from(bounds.size.width), f64::from(bounds.size.height));
        let frame = NSRect::new(
            NSPoint::new(x, parent_height - y - height),
            NSSize::new(width, height),
        );
        if self.view.frame() != frame {
            self.view.setFrame(frame);
        }
    }

    pub fn set_visible(&self, visible: bool) {
        if !visible {
            self.release_keyboard();
        }
        self.view.setHidden(!visible);
    }

    /// Hands the keyboard back to GPUI's view if the page has it. A page
    /// becomes first responder when clicked and keeps it, hidden or not. It
    /// then passes the keys it doesn't use up to GPUI's view, and GPUI
    /// handles each of them a second time, so typing doubles.
    pub fn release_keyboard(&self) {
        let Some(window) = self.parent.window() else {
            return;
        };
        let holds = window
            .firstResponder()
            .and_then(|responder| responder.downcast::<NSView>().ok())
            .is_some_and(|view| view.isDescendantOf(&self.view));
        if holds && !crate::overlay::make_first_responder(&window, &self.parent) {
            tracing::warn!("web: GPUI's view refused first responder");
        }
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        self.release_keyboard();
        self.view.removeFromSuperview();
    }
}
