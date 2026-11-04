//! The page API in the helper (ADR-0102): its grant and rule cross to the renderer in
//! the browser's `extra_info`, its updates as process messages from the browser process,
//! and the renderer defines `window.jstream` in each main-frame context the rule covers.
//!
//! **Browser process.** [`extra_info`] describes a granted page when its browser is made,
//! and [`message`] turns an update into the process message its main frame is sent.
//! Every process message a renderer sends is dropped (`handlers.rs`), so nothing reaches
//! the engine from a page.
//!
//! **Renderer process.** [`renderer`] is the render-process handler. It keeps, per
//! browser, the grant and rule from `extra_info` and the page's merged updates
//! ([`PageCache`]); when a JavaScript context is made in a main frame whose URL the rule
//! covers, it evaluates the shim there, keeps the update function it returns, and feeds
//! it everything the cache holds; each later update goes to it as it arrives. CEF calls
//! all of these on the renderer's main thread, so the state is that thread's own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::raw::c_int;

use cef::*;

use crate::page_api::{ApiHome, PageCache, shim, update_json};
use crate::{PageApi, PageData};

/// The process message an update travels in.
const MESSAGE: &str = "jstream.page_data";

/// `extra_info` keys.
const KEY_API: &str = "jstream.page_api";
const KEY_SCENES: &str = "jstream.page_api.scenes";
const KEY_VALUES: &str = "jstream.page_api.values";
const KEY_HOME_KIND: &str = "jstream.page_api.home_kind";
const KEY_HOME: &str = "jstream.page_api.home";

/// The `extra_info` for a page granted `api` whose documents `home` covers.
pub(crate) fn extra_info(api: PageApi, home: &ApiHome) -> Option<DictionaryValue> {
    let info = dictionary_value_create()?;
    let (kind, value) = home.to_parts();
    info.set_bool(Some(&KEY_API.into()), 1);
    info.set_bool(Some(&KEY_SCENES.into()), c_int::from(api.scenes));
    info.set_bool(Some(&KEY_VALUES.into()), c_int::from(api.values));
    info.set_string(Some(&KEY_HOME_KIND.into()), Some(&kind.into()));
    info.set_string(Some(&KEY_HOME.into()), Some(&value.into()));
    Some(info)
}

/// `data` as the process message its page's main frame is sent: the clock (a flag and
/// two numbers), the visibility and scenes (empty for none: JSON text is never empty),
/// then each value's name and JSON text (empty for one removed).
pub(crate) fn message(data: &PageData) -> Option<ProcessMessage> {
    let message = process_message_create(Some(&MESSAGE.into()))?;
    let list = message.argument_list()?;
    let (t, lead) = data.clock.unwrap_or((0.0, 0.0));
    list.set_bool(0, c_int::from(data.clock.is_some()));
    list.set_double(1, t);
    list.set_double(2, lead);
    list.set_string(3, Some(&data.source.as_deref().unwrap_or_default().into()));
    list.set_string(4, Some(&data.scenes.as_deref().unwrap_or_default().into()));
    for (n, (name, value)) in data.values.iter().enumerate() {
        list.set_string(5 + 2 * n, Some(&name.as_str().into()));
        list.set_string(
            6 + 2 * n,
            Some(&value.as_deref().unwrap_or_default().into()),
        );
    }
    Some(message)
}

/// The update [`message`] wrote, or `None` for any other message.
fn read_message(message: &ProcessMessage) -> Option<PageData> {
    if CefString::from(&message.name()).to_string() != MESSAGE {
        return None;
    }
    let list = message.argument_list()?;
    let text = |index: usize| -> Option<String> {
        let text = CefString::from(&list.string(index)).to_string();
        (!text.is_empty()).then_some(text)
    };
    let mut data = PageData {
        clock: (list.bool(0) == 1).then(|| (list.double(1), list.double(2))),
        source: text(3),
        scenes: text(4),
        values: Vec::new(),
    };
    let mut index = 5;
    while index + 1 < list.size() {
        if let Some(name) = text(index) {
            data.values.push((name, text(index + 1)));
        }
        index += 2;
    }
    Some(data)
}

/// One browser's page API in the renderer.
struct Granted {
    api: PageApi,
    home: ApiHome,
    cache: PageCache,
    /// The main frame's current context and the shim's update function in it, while that
    /// frame shows a document the rule covers.
    feed: Option<(V8Context, V8Value)>,
}

thread_local! {
    /// The renderer's granted browsers, by browser ID.
    static GRANTED: RefCell<HashMap<c_int, Granted>> = RefCell::new(HashMap::new());
}

/// Hands `data` to the shim's update function in `context`.
fn feed(context: &V8Context, function: &V8Value, data: &PageData) {
    if context.enter() != 1 {
        return;
    }
    let text = update_json(data);
    if let Some(argument) = v8_value_create_string(Some(&text.as_str().into())) {
        // A throw is the page's own doing (a listener's errors are rethrown later by the
        // shim); the update has gone in either way.
        let _ = function.execute_function(None, Some(&[Some(argument)]));
    }
    context.exit();
}

wrap_render_process_handler! {
    pub(crate) struct PageApiRenderer;

    impl RenderProcessHandler {
        fn on_browser_created(
            &self,
            browser: Option<&mut Browser>,
            extra_info: Option<&mut DictionaryValue>,
        ) {
            let (Some(browser), Some(info)) = (browser, extra_info) else {
                return;
            };
            if info.bool(Some(&KEY_API.into())) != 1 {
                return;
            }
            let read = |key: &str| CefString::from(&info.string(Some(&key.into()))).to_string();
            let Some(home) = ApiHome::from_parts(&read(KEY_HOME_KIND), &read(KEY_HOME)) else {
                return;
            };
            let api = PageApi {
                scenes: info.bool(Some(&KEY_SCENES.into())) == 1,
                values: info.bool(Some(&KEY_VALUES.into())) == 1,
            };
            let id = browser.identifier();
            GRANTED.with_borrow_mut(|granted| {
                granted.insert(
                    id,
                    Granted {
                        api,
                        home,
                        cache: PageCache::default(),
                        feed: None,
                    },
                );
            });
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                let id = browser.identifier();
                GRANTED.with_borrow_mut(|granted| granted.remove(&id));
            }
        }

        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let (Some(browser), Some(frame), Some(context)) = (browser, frame, context) else {
                return;
            };
            // Never in a subframe or a popup's context, nor for a document the rule
            // doesn't cover.
            if frame.is_main() != 1 {
                return;
            }
            let url = CefString::from(&frame.url()).to_string();
            let id = browser.identifier();
            // No borrow is held while JavaScript runs: a listener could make a frame,
            // whose context this handler is then called for.
            let Some((api, full)) = GRANTED.with_borrow_mut(|granted| {
                let page = granted.get_mut(&id)?;
                page.feed = None;
                page.home.covers(&url).then(|| (page.api, page.cache.full()))
            }) else {
                return;
            };
            let code = shim(api);
            let mut function = None;
            let mut exception = None;
            let evaluated = context.eval(
                Some(&code.as_str().into()),
                Some(&"jstream:page-api".into()),
                1,
                Some(&mut function),
                Some(&mut exception),
            ) == 1;
            let Some(function) = function.filter(|f| evaluated && f.is_function() == 1) else {
                return;
            };
            feed(context, &function, &full);
            GRANTED.with_borrow_mut(|granted| {
                if let Some(page) = granted.get_mut(&id) {
                    page.feed = Some((context.clone(), function));
                }
            });
        }

        fn on_context_released(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _context: Option<&mut V8Context>,
        ) {
            let (Some(browser), Some(frame)) = (browser, frame) else {
                return;
            };
            if frame.is_main() == 1 {
                let id = browser.identifier();
                GRANTED.with_borrow_mut(|granted| {
                    if let Some(page) = granted.get_mut(&id) {
                        page.feed = None;
                    }
                });
            }
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> c_int {
            let (Some(browser), Some(message)) = (browser, message) else {
                return 0;
            };
            // Only the browser process sends updates.
            if source_process != ProcessId::BROWSER {
                return 0;
            }
            let Some(data) = read_message(message) else {
                return 0;
            };
            let id = browser.identifier();
            let feeding = GRANTED.with_borrow_mut(|granted| {
                let page = granted.get_mut(&id)?;
                page.cache.merge(&data);
                page.feed.clone()
            });
            if let Some((context, function)) = feeding {
                feed(&context, &function, &data);
            }
            1
        }
    }
}

/// The render-process handler.
pub(crate) fn renderer() -> RenderProcessHandler {
    PageApiRenderer::new()
}
