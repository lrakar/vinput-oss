//! What screen readers and other assistive tools see: a UI Automation
//! provider for the window and everything drawn in it.
//!
//! The window draws its own controls, so Windows knows nothing about them
//! unless told. Here the window is a tree of two levels: the window itself,
//! and under it every control and every piece of text, in the order they are
//! laid out. Each control says what it is called, what kind it is, where it
//! is, and can be worked through the patterns its kind has: a button is
//! invoked, a switch toggled, a list expanded, a field given a value.
//!
//! Providers are handed to other processes and may outlive the window, so
//! they hold it weakly, and name what they stand for by its place in the
//! layout together with the [shape](crate::stage::Stage::shape) the page had
//! when they were made. A provider whose window is gone, or whose page has
//! become another one since, says its element is not available, and
//! whoever listens is told that the children of the window changed.
//!
//! Everything here runs on the thread of the window. The providers say so
//! to UI Automation (`ProviderOptions_UseComThreading`), which then calls
//! them through COM on the thread that made them; that takes a thread COM
//! calls back on, so a window whose thread is not one hands out no providers
//! at all (`Host::accessible`). A provider checks the thread again before it
//! touches the window, should anything call it from elsewhere regardless.
#![allow(unsafe_code)]
#![expect(
    non_upper_case_globals,
    reason = "the names Windows gives its constants, matched on here"
)]
#![expect(
    clippy::ref_as_ptr,
    clippy::inline_always,
    reason = "in the code the `implement` macro of the windows crate writes"
)]

use std::cell::RefCell;
use std::rc::Weak;
use std::thread::{self, ThreadId};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::{
    ExpandCollapseState, ExpandCollapseState_Collapsed, ExpandCollapseState_Expanded,
    IExpandCollapseProvider, IExpandCollapseProvider_Impl, IInvokeProvider, IInvokeProvider_Impl,
    IRawElementProviderFragment, IRawElementProviderFragment_Impl, IRawElementProviderFragmentRoot,
    IRawElementProviderFragmentRoot_Impl, IRawElementProviderSimple,
    IRawElementProviderSimple_Impl, IToggleProvider, IToggleProvider_Impl, IValueProvider,
    IValueProvider_Impl, NavigateDirection, NavigateDirection_FirstChild,
    NavigateDirection_LastChild, NavigateDirection_NextSibling, NavigateDirection_Parent,
    NavigateDirection_PreviousSibling, ProviderOptions, ProviderOptions_ServerSideProvider,
    ProviderOptions_UseComThreading, StructureChangeType_ChildrenInvalidated, ToggleState,
    ToggleState_Off, ToggleState_On, UIA_AutomationFocusChangedEventId, UIA_AutomationIdPropertyId,
    UIA_ButtonControlTypeId, UIA_CONTROLTYPE_ID, UIA_ComboBoxControlTypeId,
    UIA_ControlTypePropertyId, UIA_E_ELEMENTNOTAVAILABLE, UIA_EditControlTypeId,
    UIA_ExpandCollapsePatternId, UIA_HasKeyboardFocusPropertyId, UIA_HelpTextPropertyId,
    UIA_HyperlinkControlTypeId, UIA_InvokePatternId, UIA_IsContentElementPropertyId,
    UIA_IsControlElementPropertyId, UIA_IsEnabledPropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_IsOffscreenPropertyId, UIA_IsPasswordPropertyId, UIA_ListItemControlTypeId,
    UIA_NamePropertyId, UIA_PATTERN_ID, UIA_PROPERTY_ID, UIA_TextControlTypeId,
    UIA_TogglePatternId, UIA_ValuePatternId, UiaAppendRuntimeId, UiaClientsAreListening,
    UiaHostProviderFromHwnd, UiaRaiseAutomationEvent, UiaRaiseStructureChangedEvent, UiaRect,
    UiaReturnRawElementProvider,
};
use windows::core::{
    BOOL, BSTR, Error, IUnknown, IUnknownImpl, Interface, PCWSTR, Result, implement,
};

use crate::geometry::Rect;
use crate::layout::{Item, Kind, Region};
use crate::shell::Frame;
use crate::window::Host;

/// What a provider stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The control with this index.
    Control(usize),
    /// A piece of text: whether it is in the part that scrolls, and its
    /// index among what is painted there.
    Text(bool, usize),
}

/// What is known about a part, gathered in one borrow of the window.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about a control"
)]
struct Facts {
    name: String,
    kind: UIA_CONTROLTYPE_ID,
    /// Where it is in the window, in units.
    rect: Rect,
    enabled: bool,
    focusable: bool,
    focused: bool,
    id: Option<&'static str>,
    help: String,
    value: Option<String>,
    writable: bool,
    secret: bool,
    switch: Option<bool>,
    open: Option<bool>,
    invokes: bool,
    /// Whether it is scrolled out of sight.
    hidden: bool,
}

/// The text an item says, if it says any.
fn said(item: &Item) -> Option<(&str, Rect)> {
    match item {
        Item::Text { text, rect, .. } | Item::Badge { text, rect, .. }
            if text.chars().any(char::is_alphanumeric) =>
        {
            Some((text, *rect))
        }
        _ => None,
    }
}

/// Everything under the window, in the order it is laid out.
fn outline(frame: &Frame) -> Vec<Part> {
    let parts = |body: bool, items: &[Item]| {
        items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item {
                Item::Control(control) => Some(Part::Control(*control)),
                other => said(other).map(|_| Part::Text(body, index)),
            })
            .collect::<Vec<_>>()
    };
    let mut all = parts(false, &frame.fixed);
    all.extend(parts(true, &frame.body));
    all
}

fn facts(host: &Host, part: Part) -> Option<Facts> {
    let stage = &host.stage;
    let frame = stage.frame();
    let plain = |name: &str, rect: Rect| Facts {
        name: name.to_owned(),
        kind: UIA_TextControlTypeId,
        rect,
        enabled: true,
        focusable: false,
        focused: false,
        id: None,
        help: String::new(),
        value: None,
        writable: false,
        secret: false,
        switch: None,
        open: None,
        invokes: false,
        hidden: false,
    };
    let index = match part {
        Part::Text(body, index) => {
            let items = if body { &frame.body } else { &frame.fixed };
            let (text, rect) = said(items.get(index)?)?;
            let dy = if body { -stage.look().scroll } else { 0.0 };
            let mut facts = plain(text, rect.moved(0.0, dy));
            facts.hidden = body && !facts.rect.touches(frame.viewport);
            return Some(facts);
        }
        Part::Control(index) => index,
    };
    let control = frame.controls.get(index)?;
    let mut facts = plain(control.name(), stage.rect_of(index)?);
    facts.enabled = control.enabled;
    facts.focusable = control.enabled;
    facts.focused = stage.focused() == Some(index);
    facts.id = control.key;
    control
        .tip()
        .unwrap_or_default()
        .clone_into(&mut facts.help);
    match &control.kind {
        Kind::Button { .. } | Kind::Row { .. } => {
            facts.kind = UIA_ButtonControlTypeId;
            facts.invokes = true;
        }
        Kind::Link { .. } => {
            facts.kind = UIA_HyperlinkControlTypeId;
            facts.invokes = true;
        }
        Kind::Nav { .. } => {
            facts.kind = UIA_ListItemControlTypeId;
            facts.invokes = true;
        }
        Kind::Toggle { on, .. } => {
            facts.kind = UIA_ButtonControlTypeId;
            facts.switch = Some(*on);
        }
        Kind::Choice {
            items, selected, ..
        } => {
            facts.kind = UIA_ComboBoxControlTypeId;
            facts.value = Some(items.get(*selected).cloned().unwrap_or_default());
            facts.open = Some(stage.look().open == Some(index));
        }
        Kind::Field { text, secret, .. } => {
            facts.kind = UIA_EditControlTypeId;
            facts.secret = *secret;
            facts.writable = control.enabled;
            facts.value = Some(if *secret { String::new() } else { text.clone() });
        }
        Kind::Hotkey { text, .. } => {
            facts.kind = UIA_ButtonControlTypeId;
            facts.value = Some(text.clone());
            facts.invokes = true;
        }
    }
    // Scrolled out of sight; it is brought back when it gets the focus.
    facts.hidden = control.region == Region::Body && !facts.rect.touches(frame.viewport);
    Some(facts)
}

fn gone() -> Error {
    // The code is a failure `HRESULT`; reinterpreting its bits as signed is
    // how such codes are written.
    Error::from_hresult(windows::core::HRESULT(
        UIA_E_ELEMENTNOTAVAILABLE.cast_signed(),
    ))
}

/// The window a provider belongs to, held so that the provider may outlive
/// it, and the thread that alone may touch it.
#[derive(Clone)]
struct Link {
    host: Weak<RefCell<Host>>,
    hwnd: HWND,
    thread: ThreadId,
}

impl Link {
    fn new(host: &Weak<RefCell<Host>>, hwnd: HWND) -> Self {
        Self {
            host: host.clone(),
            hwnd,
            thread: thread::current().id(),
        }
    }

    /// Reads from the window, if it is still there and not in the middle of
    /// something.
    fn read<R>(&self, with: impl FnOnce(&Host) -> Option<R>) -> Result<R> {
        if thread::current().id() != self.thread {
            return Err(gone());
        }
        let host = self.host.upgrade().ok_or_else(gone)?;
        let host = host.try_borrow().map_err(|_| gone())?;
        with(&host).ok_or_else(gone)
    }

    /// Does something to the window, as the user could.
    fn act(&self, with: impl FnOnce(&mut Host)) -> Result<()> {
        if thread::current().id() != self.thread {
            return Err(gone());
        }
        let host = self.host.upgrade().ok_or_else(gone)?;
        let mut host = host.try_borrow_mut().map_err(|_| gone())?;
        with(&mut host);
        Ok(())
    }

    /// The shape of the page right now.
    fn shape(&self) -> Result<u64> {
        self.read(|host| Some(host.stage.shape()))
    }

    fn element(&self, part: Part, shape: u64) -> IRawElementProviderFragment {
        Element {
            link: self.clone(),
            part,
            shape,
        }
        .into()
    }

    fn root(&self) -> IRawElementProviderFragmentRoot {
        Root { link: self.clone() }.into()
    }
}

/// A list of numbers the way UI Automation takes one.
fn numbers(values: &[i32]) -> Result<*mut SAFEARRAY> {
    let count = u32::try_from(values.len()).map_err(|_| gone())?;
    // SAFETY: the array is made for `count` 32-bit numbers and each index
    // written is below `count`; the pointer to each value is valid for its
    // call. Ownership of the array passes to the caller, as COM expects.
    unsafe {
        let array = SafeArrayCreateVector(VT_I4, 0, count);
        if array.is_null() {
            return Err(gone());
        }
        for (index, value) in values.iter().enumerate() {
            let index = i32::try_from(index).map_err(|_| gone())?;
            SafeArrayPutElement(array, &raw const index, std::ptr::from_ref(value).cast())?;
        }
        Ok(array)
    }
}

/// The provider for the window itself.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct Root {
    link: Link,
}

/// The provider for one control or piece of text.
#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider,
    IToggleProvider,
    IValueProvider,
    IExpandCollapseProvider
)]
struct Element {
    link: Link,
    part: Part,
    /// The shape the page had when this was made; on a page of another
    /// shape the same place stands for something else.
    shape: u64,
}

const OPTIONS: ProviderOptions =
    ProviderOptions(ProviderOptions_ServerSideProvider.0 | ProviderOptions_UseComThreading.0);

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(OPTIONS)
    }

    fn GetPatternProvider(&self, _pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        Err(Error::empty())
    }

    fn GetPropertyValue(&self, _property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        // The window's own provider, which Windows supplies, answers.
        Ok(VARIANT::default())
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // SAFETY: a plain call with the handle of the window this stands for.
        unsafe { UiaHostProviderFromHwnd(self.link.hwnd) }
    }
}

impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let (part, shape) = self.link.read(|host| {
            let all = outline(host.stage.frame());
            let part = match direction {
                NavigateDirection_FirstChild => all.first().copied(),
                NavigateDirection_LastChild => all.last().copied(),
                _ => None,
            };
            Some((part, host.stage.shape()))
        })?;
        part.map(|part| self.link.element(part, shape))
            .ok_or_else(Error::empty)
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // The window's own provider has one already.
        Err(Error::empty())
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(UiaRect::default())
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Err(Error::empty())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.link.root())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        #[expect(clippy::cast_possible_truncation, reason = "a position on a screen")]
        let mut point = POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        // SAFETY: `point` is valid for the call.
        let _ = unsafe { ScreenToClient(self.link.hwnd, &raw mut point) };
        let (part, shape) = self.link.read(|host| {
            let at = host.units(point);
            let stage = &host.stage;
            let part = stage.control_at(at).map(Part::Control).or_else(|| {
                outline(stage.frame()).into_iter().find(|part| {
                    matches!(part, Part::Text(..))
                        && facts(host, *part)
                            .is_some_and(|facts| !facts.hidden && facts.rect.contains(at))
                })
            });
            Some((part, stage.shape()))
        })?;
        match part {
            Some(part) => Ok(self.link.element(part, shape)),
            None => self.link.root().cast(),
        }
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let (focused, shape) = self
            .link
            .read(|host| Some((host.stage.focused(), host.stage.shape())))?;
        focused
            .map(|index| self.link.element(Part::Control(index), shape))
            .ok_or_else(Error::empty)
    }
}

impl Element_Impl {
    /// Reads from the window, if the page is still the one this was made
    /// for.
    fn read<R>(&self, with: impl FnOnce(&Host) -> Option<R>) -> Result<R> {
        let shape = self.shape;
        self.link
            .read(|host| (host.stage.shape() == shape).then(|| with(host)).flatten())
    }

    fn facts(&self) -> Result<Facts> {
        let part = self.part;
        self.read(|host| facts(host, part))
    }

    fn control(&self) -> Result<usize> {
        match self.part {
            Part::Control(index) => Ok(index),
            Part::Text(..) => Err(gone()),
        }
    }

    /// Works the control as a click does.
    fn press(&self) -> Result<()> {
        let index = self.control()?;
        if !self.facts()?.enabled {
            return Err(gone());
        }
        self.link.act(|host| {
            let effects = host.stage.press(index);
            host.perform(effects);
        })
    }
}

impl IRawElementProviderSimple_Impl for Element_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(OPTIONS)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        let facts = self.facts()?;
        let has = match pattern {
            UIA_InvokePatternId => facts.invokes,
            UIA_TogglePatternId => facts.switch.is_some(),
            UIA_ValuePatternId => facts.value.is_some(),
            UIA_ExpandCollapsePatternId => facts.open.is_some(),
            _ => false,
        };
        if has {
            self.to_interface::<IRawElementProviderSimple>().cast()
        } else {
            Err(Error::empty())
        }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let facts = self.facts()?;
        Ok(match property {
            UIA_ControlTypePropertyId => VARIANT::from(facts.kind.0),
            UIA_NamePropertyId => VARIANT::from(facts.name.as_str()),
            UIA_HelpTextPropertyId if !facts.help.is_empty() => VARIANT::from(facts.help.as_str()),
            UIA_AutomationIdPropertyId => match facts.id {
                Some(id) => VARIANT::from(id),
                None => VARIANT::default(),
            },
            UIA_IsEnabledPropertyId => VARIANT::from(facts.enabled),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(facts.focusable),
            UIA_HasKeyboardFocusPropertyId => VARIANT::from(facts.focused),
            UIA_IsPasswordPropertyId => VARIANT::from(facts.secret),
            UIA_IsOffscreenPropertyId => VARIANT::from(facts.hidden),
            UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => VARIANT::from(true),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(Error::empty())
    }
}

impl IRawElementProviderFragment_Impl for Element_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        if direction == NavigateDirection_Parent {
            return self.link.root().cast();
        }
        let part = self.part;
        let next = self.read(|host| {
            let all = outline(host.stage.frame());
            let at = all.iter().position(|other| *other == part)?;
            Some(match direction {
                NavigateDirection_NextSibling => all.get(at + 1).copied(),
                NavigateDirection_PreviousSibling => {
                    at.checked_sub(1).and_then(|at| all.get(at).copied())
                }
                _ => None,
            })
        })?;
        next.map(|part| self.link.element(part, self.shape))
            .ok_or_else(Error::empty)
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let (kind, index) = match self.part {
            Part::Control(index) => (1, index),
            Part::Text(false, index) => (2, index),
            Part::Text(true, index) => (3, index),
        };
        // The shape is part of the name: the same place on another page is
        // another element. Only its low bits fit, which is plenty to tell
        // one page from the next.
        let shape = i32::try_from(self.shape & 0x7FFF_FFFF).map_err(|_| gone())?;
        numbers(&[
            UiaAppendRuntimeId.cast_signed(),
            kind,
            i32::try_from(index).map_err(|_| gone())?,
            shape,
        ])
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let part = self.part;
        self.read(|host| {
            let facts = facts(host, part)?;
            if facts.hidden {
                return Some(UiaRect::default());
            }
            let rect = host.on_screen(facts.rect);
            Some(UiaRect {
                left: f64::from(rect.left),
                top: f64::from(rect.top),
                width: f64::from(rect.right - rect.left),
                height: f64::from(rect.bottom - rect.top),
            })
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Err(Error::empty())
    }

    fn SetFocus(&self) -> Result<()> {
        let index = self.control()?;
        if !self.facts()?.focusable {
            return Err(gone());
        }
        self.link.act(|host| {
            let effects = host.stage.focus_on(index);
            host.perform(effects);
        })
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.link.root())
    }
}

impl IInvokeProvider_Impl for Element_Impl {
    fn Invoke(&self) -> Result<()> {
        self.press()
    }
}

impl IToggleProvider_Impl for Element_Impl {
    fn Toggle(&self) -> Result<()> {
        self.press()
    }

    fn ToggleState(&self) -> Result<ToggleState> {
        match self.facts()?.switch {
            Some(true) => Ok(ToggleState_On),
            Some(false) => Ok(ToggleState_Off),
            None => Err(gone()),
        }
    }
}

impl IValueProvider_Impl for Element_Impl {
    fn SetValue(&self, value: &PCWSTR) -> Result<()> {
        let index = self.control()?;
        if !self.facts()?.writable {
            return Err(gone());
        }
        // SAFETY: UI Automation passes a NUL-terminated string that is valid
        // for the call.
        let text = unsafe { value.to_string() }.map_err(|_| gone())?;
        self.link.act(|host| {
            let effects = host.stage.write(index, text);
            host.perform(effects);
        })
    }

    fn Value(&self) -> Result<BSTR> {
        self.facts()?
            .value
            .map(|value| BSTR::from(value.as_str()))
            .ok_or_else(gone)
    }

    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL::from(!self.facts()?.writable))
    }
}

impl IExpandCollapseProvider_Impl for Element_Impl {
    fn Expand(&self) -> Result<()> {
        if self.facts()?.open == Some(false) {
            self.press()?;
        }
        Ok(())
    }

    fn Collapse(&self) -> Result<()> {
        if self.facts()?.open == Some(true) {
            self.link.act(|host| {
                let effects = host.stage.close_menu();
                host.perform(effects);
            })?;
        }
        Ok(())
    }

    fn ExpandCollapseState(&self) -> Result<ExpandCollapseState> {
        match self.facts()?.open {
            Some(true) => Ok(ExpandCollapseState_Expanded),
            Some(false) => Ok(ExpandCollapseState_Collapsed),
            None => Err(gone()),
        }
    }
}

/// Answers `WM_GETOBJECT`: hands UI Automation the provider for the window.
pub(crate) fn answer(
    host: &Weak<RefCell<Host>>,
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let provider: Result<IRawElementProviderSimple> = Link::new(host, hwnd).root().cast();
    match provider {
        // SAFETY: the provider is a live object and the parameters are those
        // of the message being answered.
        Ok(provider) => unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &provider) },
        Err(_) => LRESULT(0),
    }
}

/// Tells UI Automation that the window is going away, so that it lets go of
/// the providers it holds.
pub(crate) fn disconnect(hwnd: HWND) {
    // SAFETY: with no provider this call only releases what UI Automation
    // holds for the window.
    unsafe {
        UiaReturnRawElementProvider(
            hwnd,
            WPARAM(0),
            LPARAM(0),
            None::<&IRawElementProviderSimple>,
        );
    }
}

/// Tells whoever listens that the control with this index has the keyboard
/// focus now. Called outside any borrow of the window: UI Automation asks
/// the provider about the control while it is told.
pub(crate) fn focus_moved(host: &Weak<RefCell<Host>>, hwnd: HWND, index: usize) {
    let link = Link::new(host, hwnd);
    let Ok(shape) = link.shape() else {
        return;
    };
    // SAFETY: a plain query, and raising an event for a live provider.
    unsafe {
        if !UiaClientsAreListening().as_bool() {
            return;
        }
        let provider: Result<IRawElementProviderSimple> =
            link.element(Part::Control(index), shape).cast();
        if let Ok(provider) = provider {
            let _ = UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId);
        }
    }
}

/// Tells whoever listens that the window is made of other things now, so
/// that they ask again instead of holding on to elements of the page that
/// was. Called outside any borrow of the window, like [`focus_moved`].
pub(crate) fn reshaped(host: &Weak<RefCell<Host>>, hwnd: HWND) {
    // SAFETY: a plain query, and raising an event for a live provider with
    // no runtime id, which this kind of change does not take.
    unsafe {
        if !UiaClientsAreListening().as_bool() {
            return;
        }
        let provider: Result<IRawElementProviderSimple> = Link::new(host, hwnd).root().cast();
        if let Ok(provider) = provider {
            let _ = UiaRaiseStructureChangedEvent(
                &provider,
                StructureChangeType_ChildrenInvalidated,
                std::ptr::null_mut(),
                0,
            );
        }
    }
}
