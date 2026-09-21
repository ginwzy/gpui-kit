---
title: Root View
description: 使用 Root 视图为窗口启用主题、通知、对话框及其他 GPUI Component 功能。
example: false
---

# Root View

[Root] 是 GPUI Kit 窗口的根视图。它持有窗口的对话框、抽屉和通知并把它们渲染在所包裹的视图之上，也承载 tooltip 与菜单。`gpui_kit::open_window` 会替你创建它；只有窗口需要定制 `Root` 时才会直接接触它。

下面这份完整的 **Tested consumer recipe** 在隔离的 `gpui-kit` 消费者工作区中编译。它先初始化 GPUI Kit，再打开一个以 `Root` 包裹应用视图为根的窗口。

<!-- recipe:bootstrap:start -->
```rust
use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowOptions, div,
};

pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            // The window's root view is a `Root` wrapping the view, which
            // renders dialogs, sheets and notifications above it.
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                cx.new(|_| BootstrapView)
            })
            .expect("failed to open window");
        });
}

struct BootstrapView;

impl Render for BootstrapView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("My application")
    }
}
```
<!-- recipe:bootstrap:end -->

## 定制 Root

`gpui_kit::open_window` 就是 `cx.open_window` 加上 `Root` 包裹。窗口需要配置 `Root` 时自己构造它——例如 layer-shell 全屏窗口不该画 GPUI Component 的客户端窗口边框，用 `bordered(false)`：

```rs
cx.open_window(WindowOptions::default(), |window, cx| {
    let view = cx.new(|_| MyApp);
    cx.new(|cx| Root::new(view, window, cx).bordered(false))
})
```

不论怎么构造，`Root` 都必须是窗口的根视图：`window.open_dialog`、`open_sheet`、`push_notification` 把状态存在它上面，缺少它时会 panic 并指向本页。

`open_window` 同时返回窗口和视图，所以必须在窗口内构造的视图（比如它持有 `InputState`）也能留住句柄：

```rust
let (window, editor) = gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
    cx.new(|cx| Editor::new(window, cx))
})?;
```

## 默认快捷键

`gpui_kit::init` 把平台的退出快捷键——macOS 上 `cmd-q`，Windows 与 Linux 上 `alt-f4`——绑定到 `gpui_kit::base::actions::Quit` 退出应用，所以每个窗口都响应它。macOS 上 `cmd-w` 绑定到 `gpui_kit::base::actions::CloseWindow`，像 `File › Close` 一样关闭当前窗口（Windows 与 Linux 用 `alt-f4` 关窗口）。两者都在 `gpui-base`：它们是窗口行为，不是样式。想在退出或关闭前确认，把同一快捷键绑到你自己的 action 上即可，后绑定的优先：

```rust
cx.bind_keys([KeyBinding::new("cmd-q", ConfirmQuit, None)]);
```

## 浮层

对话框、抽屉和通知渲染在 `Root` 放在视图之上的图层里，所以一个完全不提及它们的视图也能显示它们。应用想把某一层放在自己树里的别处——比如放在自己的标题栏之下，或放在一个必须压在最上面的 HUD 之下——就自己渲染那一层，`Root` 会跳过它：

- [Root::render_dialog_layer](https://docs.rs/gpui-component/latest/gpui_component/struct.Root.html#method.render_dialog_layer) - 打开的对话框
- [Root::render_sheet_layer](https://docs.rs/gpui-component/latest/gpui_component/struct.Root.html#method.render_sheet_layer) - 打开的抽屉
- [Root::render_notification_layer](https://docs.rs/gpui-component/latest/gpui_component/struct.Root.html#method.render_notification_layer) - 通知列表

```rust
impl Render for MyApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(self.content.clone())
            // 有意放在这里；Root 不会再加第二份。
            .children(Root::render_dialog_layer(window, cx))
            .child(self.hud.clone())
    }
}
```

没有内容可显示时它们返回 `None`，所以示例用的是 `children`。

[Root]: https://docs.rs/gpui-component/latest/gpui_component/root/struct.Root.html
