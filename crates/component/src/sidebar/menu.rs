use crate::{
    ActiveTheme as _, Collapsible, FocusableExt, Icon, IconName, Placement, Sizable as _,
    StyledExt, ThemeStyled as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt, PopupMenu},
    sidebar::SidebarItem,
    tooltip::{ManagedTooltipExt as _, Tooltip},
    v_flex,
};
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled, Window, div, percentage, prelude::FluentBuilder,
};
use gpui_base::TestSupportExt as _;
use std::rc::Rc;

/// Menu for the [`super::Sidebar`]
#[derive(Clone)]
pub struct SidebarMenu {
    style: StyleRefinement,
    collapsed: bool,
    items: Vec<SidebarMenuItem>,
}

impl SidebarMenu {
    /// Create a new SidebarMenu
    pub fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            items: Vec::new(),
            collapsed: false,
        }
    }

    /// Add a [`SidebarMenuItem`] child menu item to the sidebar menu.
    ///
    /// See also [`SidebarMenu::children`].
    pub fn child(mut self, child: impl Into<SidebarMenuItem>) -> Self {
        self.items.push(child.into());
        self
    }

    /// Add multiple [`SidebarMenuItem`] child menu items to the sidebar menu.
    pub fn children(
        mut self,
        children: impl IntoIterator<Item = impl Into<SidebarMenuItem>>,
    ) -> Self {
        self.items = children.into_iter().map(Into::into).collect();
        self
    }
}

impl Collapsible for SidebarMenu {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for SidebarMenu {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let id = id.into();

        v_flex()
            .gap_2()
            .refine_style(&self.style)
            .children(self.items.into_iter().enumerate().map(|(ix, item)| {
                let id = SharedString::from(format!("{}-{}", id, ix));
                item.collapsed(self.collapsed)
                    .render(id, window, cx)
                    .into_any_element()
            }))
    }
}

impl Styled for SidebarMenu {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// Menu item for the [`SidebarMenu`]
#[derive(Clone)]
pub struct SidebarMenuItem {
    icon: Option<Icon>,
    label: SharedString,
    label_style: StyleRefinement,
    style: StyleRefinement,
    handler: Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    active: bool,
    default_open: bool,
    click_to_open: bool,
    collapsed: bool,
    click_to_toggle: bool,
    children: Vec<Self>,
    suffix: Option<Rc<dyn Fn(&mut Window, &mut App) -> AnyElement + 'static>>,
    disabled: bool,
    context_menu: Option<Rc<dyn Fn(PopupMenu, &mut Window, &mut App) -> PopupMenu + 'static>>,
    focus_ring_enabled: bool,
    tab_index: isize,
    tab_stop: bool,
}

impl SidebarMenuItem {
    /// Create a new [`SidebarMenuItem`] with a label.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            icon: None,
            label: label.into(),
            label_style: StyleRefinement::default(),
            style: StyleRefinement::default(),
            handler: Rc::new(|_, _, _| {}),
            active: false,
            collapsed: false,
            default_open: false,
            click_to_open: false,
            click_to_toggle: false,
            children: Vec::new(),
            suffix: None,
            disabled: false,
            context_menu: None,
            focus_ring_enabled: true,
            tab_index: 0,
            tab_stop: true,
        }
    }

    /// Set the icon for the menu item
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Set the style for the label
    pub fn label_style(mut self, style: StyleRefinement) -> Self {
        self.label_style = style;
        self
    }

    /// Set the active state of the menu item
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Add a click handler to the menu item
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handler = Rc::new(handler);
        self
    }

    /// Set the collapsed state of the menu item
    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    /// Set the default open state of the Submenu, default is `false`.
    ///
    /// This only used on initial render, the internal state will be used afterwards.
    pub fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }

    /// Set whether clicking the menu item open the submenu.
    ///
    /// Default is `false`.
    ///
    /// If `false` we only handle open/close via the caret button.
    pub fn click_to_open(mut self, click_to_open: bool) -> Self {
        self.click_to_open = click_to_open;
        self
    }

    /// Set whether clicking the menu item toggles the submenu.
    ///
    /// If click_to_open is `true`, this has no effect.
    ///
    /// Default is `false`.
    pub fn click_to_toggle(mut self, click_to_toggle: bool) -> Self {
        self.click_to_toggle = click_to_toggle;
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = impl Into<Self>>) -> Self {
        self.children = children.into_iter().map(Into::into).collect();
        self
    }

    /// Set the suffix for the menu item.
    pub fn suffix<F, E>(mut self, builder: F) -> Self
    where
        F: Fn(&mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.suffix = Some(Rc::new(move |window, cx| {
            builder(window, cx).into_any_element()
        }));
        self
    }

    /// Set disabled flat for menu item.
    pub fn disable(mut self, disable: bool) -> Self {
        self.disabled = disable;
        self
    }

    /// Set the tab index of the menu item, used to order it in keyboard focus traversal.
    ///
    /// Default is `0`.
    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.tab_index = tab_index;
        self
    }

    /// Set whether the menu item is a tab stop, so the Tab key can reach it.
    ///
    /// Default is `true`. A focused item activates on Enter or Space, the same
    /// as a [`Button`].
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    fn is_submenu(&self) -> bool {
        self.children.len() > 0
    }

    fn collapsed_tooltip(&self) -> Option<SharedString> {
        (self.collapsed && self.icon.is_some()).then(|| self.label.clone())
    }

    /// Set the context menu for the menu item.
    pub fn context_menu(
        mut self,
        f: impl Fn(PopupMenu, &mut Window, &mut App) -> PopupMenu + 'static,
    ) -> Self {
        self.context_menu = Some(Rc::new(f));
        self
    }
}

impl FluentBuilder for SidebarMenuItem {}

impl FocusableExt for SidebarMenuItem {
    fn focus_ring(mut self, enabled: bool) -> Self {
        self.focus_ring_enabled = enabled;
        self
    }

    fn is_focus_ring_enabled(&self) -> bool {
        self.focus_ring_enabled
    }
}

impl Collapsible for SidebarMenuItem {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for SidebarMenuItem {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let click_to_open = self.click_to_open;
        let click_to_toggle = self.click_to_toggle;
        let default_open = self.default_open;
        let collapsed_tooltip = self.collapsed_tooltip();
        let id = id.into();
        let is_submenu = self.is_submenu();
        let open_state = if is_submenu {
            Some(window.use_keyed_state(id.clone(), cx, |_, _| default_open))
        } else {
            None
        };
        let handler = self.handler.clone();
        let is_collapsed = self.collapsed;
        let is_active = self.active;
        let is_hoverable = !is_active && !self.disabled;
        let is_disabled = self.disabled;
        let is_open = open_state
            .as_ref()
            .map_or(false, |s| !is_collapsed && *s.read(cx));
        // The row owns keyboard focus the way `Button` does: a keyed handle that
        // survives re-renders, Enter and Space delivered as a keyboard click.
        let focus_handle = window
            .use_keyed_state((id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let is_focused = focus_handle.is_focused(window);
        let show_focus_ring = is_focused && self.focus_ring_enabled;
        let tab_index = self.tab_index;
        let tab_stop = self.tab_stop;

        div()
            .id(id.clone())
            .test_support()
            .w_full()
            .child(
                h_flex()
                    .size_full()
                    .id("item")
                    .flex_shrink_0()
                    .p_2()
                    .gap_x_2()
                    .rounded(cx.theme().radius)
                    .text_sm()
                    .refine_style(&self.style)
                    .role(Role::Button)
                    .aria_label(self.label.clone())
                    .when(!is_disabled, |this| {
                        this.track_focus(&focus_handle.tab_index(tab_index).tab_stop(tab_stop))
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                // Keep the focus ring for keyboard navigation, as
                                // `Button` does: a pointer press does not focus.
                                window.prevent_default();
                            })
                    })
                    .when(is_hoverable, |this| {
                        this.hover(|this| {
                            this.bg(cx.theme().sidebar_accent.opacity(0.8))
                                .text_color(cx.theme().sidebar_accent_foreground)
                        })
                    })
                    .when(is_active, |this| {
                        this.font_medium()
                            .bg(cx.theme().tokens.sidebar_accent)
                            .text_color(cx.theme().sidebar_accent_foreground)
                    })
                    .when_some(self.icon.clone(), |this, icon| this.child(icon))
                    .when(is_collapsed, |this| {
                        this.justify_center().when(is_active, |this| {
                            this.bg(cx.theme().tokens.sidebar_accent)
                                .text_color(cx.theme().sidebar_accent_foreground)
                        })
                    })
                    .when(!is_collapsed, |this| {
                        this.h_7()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .gap_x_2()
                                    .justify_between()
                                    .overflow_x_hidden()
                                    .child(
                                        h_flex()
                                            .flex_1()
                                            .overflow_x_hidden()
                                            .refine_style(&self.label_style)
                                            .child(self.label.clone()),
                                    )
                                    .when_some(self.suffix.clone(), |this, suffix| {
                                        this.child(suffix(window, cx).into_any_element())
                                    }),
                            )
                            .when_some(open_state.clone(), |this, open_state| {
                                this.child(
                                    Button::new("caret")
                                        .xsmall()
                                        .ghost()
                                        .icon(
                                            Icon::new(IconName::ChevronRight)
                                                .size_4()
                                                .when(is_open, |this| {
                                                    this.rotate(percentage(90. / 360.))
                                                }),
                                        )
                                        .on_click({
                                            move |_, _, cx| {
                                                // Avoid trigger item click, just expand/collapse submenu
                                                cx.stop_propagation();
                                                open_state.update(cx, |is_open, cx| {
                                                    *is_open = !*is_open;
                                                    cx.notify();
                                                })
                                            }
                                        }),
                                )
                            })
                    })
                    .when(is_disabled, |this| {
                        this.text_color(cx.theme().muted_foreground)
                    })
                    .when(show_focus_ring, |this| this.focus_ring_style(window, cx))
                    .when(!is_disabled, |this| {
                        this.on_click({
                            let open_state = open_state.clone();
                            move |ev, window, cx| {
                                if click_to_open {
                                    if let Some(ref s) = open_state {
                                        s.update(cx, |is_open: &mut bool, cx| {
                                            *is_open = true;
                                            cx.notify();
                                        });
                                    }
                                } else if click_to_toggle {
                                    if let Some(ref s) = open_state {
                                        s.update(cx, |is_open: &mut bool, cx| {
                                            *is_open = !*is_open;
                                            cx.notify();
                                        });
                                    }
                                }
                                handler(ev, window, cx)
                            }
                        })
                    })
                    .map(|this| {
                        if let Some(tooltip) = collapsed_tooltip {
                            this.managed_tooltip_at(Placement::Right, move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                        } else {
                            this
                        }
                    })
                    .map(|this| {
                        if let Some(context_menu) = self.context_menu {
                            this.context_menu(move |menu, window, cx| {
                                context_menu(menu, window, cx)
                            })
                            .into_any_element()
                        } else {
                            this.into_any_element()
                        }
                    }),
            )
            .when(is_open, |this| {
                this.child(
                    v_flex()
                        .id("submenu")
                        .border_l_1()
                        .border_color(cx.theme().sidebar_border)
                        .gap_1()
                        .ml_3p5()
                        .pl_2p5()
                        .py_0p5()
                        .children(self.children.into_iter().enumerate().map(|(ix, item)| {
                            let id = format!("{}-{}", id, ix);
                            item.render(id, window, cx).into_any_element()
                        })),
                )
            })
    }
}

impl Styled for SidebarMenuItem {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{
        Context, KeyDownEvent, KeyUpEvent, Keystroke, Render, TestAppContext, VisualTestContext,
    };

    use super::*;
    use crate::sidebar::{Sidebar, SidebarGroup};

    /// Which items a keyboard user can reach, and which one Enter activates.
    struct MenuHarness {
        clicks: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Render for MenuHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let item = |label: &'static str| {
                let clicks = self.clicks.clone();
                SidebarMenuItem::new(label).on_click(move |_, _, _| clicks.borrow_mut().push(label))
            };

            Sidebar::new("sidebar").child(
                SidebarGroup::new("Workspace").child(
                    SidebarMenu::new()
                        .child(item("Inbox"))
                        .child(item("Archive").disable(true))
                        .child(item("Drafts").tab_stop(false))
                        .child(item("Sent")),
                ),
            )
        }
    }

    fn harness(
        cx: &mut TestAppContext,
    ) -> (&mut VisualTestContext, Rc<RefCell<Vec<&'static str>>>) {
        cx.update(crate::init);
        let clicks = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, _| MenuHarness { clicks }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, clicks)
    }

    fn activate_key(cx: &mut VisualTestContext, key: &str) {
        let keystroke = Keystroke::parse(key).unwrap();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
    }

    fn focus_next_and_activate(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.focus_next(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        activate_key(cx, "enter");
    }

    /// Tab walks the enabled tab stops in order and Enter activates the focused
    /// item; a disabled item and a `tab_stop(false)` item are skipped.
    #[gpui::test]
    fn tab_reaches_each_enabled_item_and_enter_activates_it(cx: &mut TestAppContext) {
        let (cx, clicks) = harness(cx);
        cx.update(|window, cx| assert!(window.focused(cx).is_none()));

        focus_next_and_activate(cx);
        focus_next_and_activate(cx);
        // Tab wraps: the third stop is the first item again.
        focus_next_and_activate(cx);

        assert_eq!(*clicks.borrow(), ["Inbox", "Sent", "Inbox"]);
    }

    /// Space activates a focused item the same way Enter does.
    #[gpui::test]
    fn space_activates_the_focused_item(cx: &mut TestAppContext) {
        let (cx, clicks) = harness(cx);
        cx.update(|window, cx| window.focus_next(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));

        activate_key(cx, "space");

        assert_eq!(*clicks.borrow(), ["Inbox"]);
    }

    #[test]
    fn collapsed_icon_item_uses_label_as_tooltip() {
        let item = SidebarMenuItem::new("Projects")
            .icon(Icon::default())
            .collapsed(true);

        assert_eq!(item.collapsed_tooltip().as_deref(), Some("Projects"));
    }

    #[test]
    fn expanded_or_iconless_item_has_no_collapsed_tooltip() {
        let expanded = SidebarMenuItem::new("Projects").icon(Icon::default());
        let iconless = SidebarMenuItem::new("Projects").collapsed(true);

        assert!(expanded.collapsed_tooltip().is_none());
        assert!(iconless.collapsed_tooltip().is_none());
    }
}
